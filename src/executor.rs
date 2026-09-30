use std::collections::BTreeSet;

use crate::currency::Currency;
use crate::state::{BusinessState, ExecutionOutcome, SecondState};
use crate::{
    AccountAddress, ConcurrentExecutionError, CurrencyAddress, CurrencyClaimBook, CurrencyRole,
    ExecutionError, Operation, OperationClaimId, VerifiedLegalTask,
};

#[derive(Clone)]
struct OperationExecutionContext {
    claim_id: OperationClaimId,
    expires_at: u64,
}

impl SecondState {
    pub fn execute(
        &mut self,
        task: &VerifiedLegalTask,
        now: u64,
    ) -> Result<ExecutionOutcome, ExecutionError> {
        if self.bind_task(task)? {
            return Ok(ExecutionOutcome::AlreadySucceeded);
        }

        if now > task.expires_at() {
            return Err(ExecutionError::TaskExpired);
        }

        let mut working = self.business.clone();
        let mut prerequisite = self.prerequisite.clone();

        for (index, operation) in task.operations().iter().enumerate() {
            let operation_index =
                u64::try_from(index).map_err(|_| ExecutionError::OperationIndexOverflow)?;
            self.execute_operation(
                &mut working,
                &mut prerequisite,
                operation,
                OperationExecutionContext {
                    claim_id: OperationClaimId::new(task.task_id(), operation_index),
                    expires_at: task.expires_at(),
                },
            )?;
        }

        self.business = working;
        self.prerequisite = prerequisite;
        self.mark_task_succeeded(task.task_id());

        Ok(ExecutionOutcome::Succeeded)
    }

    pub fn execute_with_claims(
        &mut self,
        task: &VerifiedLegalTask,
        now: u64,
        claims: &mut CurrencyClaimBook,
    ) -> Result<ExecutionOutcome, ConcurrentExecutionError> {
        if self.bind_task(task)? {
            claims.release_task(task.task_id());
            return Ok(ExecutionOutcome::AlreadySucceeded);
        }

        if now > task.expires_at() {
            claims.release_task(task.task_id());
            return Err(ExecutionError::TaskExpired.into());
        }

        let mut working = self.business.clone();
        let mut prerequisite = self.prerequisite.clone();
        let result =
            self.execute_operations_with_claims(&mut working, &mut prerequisite, task, claims);

        match result {
            Ok(()) => {
                self.business = working;
                self.prerequisite = prerequisite;
                self.mark_task_succeeded(task.task_id());
                claims.release_task(task.task_id());
                Ok(ExecutionOutcome::Succeeded)
            }
            Err(error) => {
                claims.release_task(task.task_id());
                Err(error)
            }
        }
    }

    fn execute_operations_with_claims(
        &mut self,
        working: &mut BusinessState,
        prerequisite: &mut crate::state::PrerequisiteState,
        task: &VerifiedLegalTask,
        claims: &mut CurrencyClaimBook,
    ) -> Result<(), ConcurrentExecutionError> {
        let mut operation_index = 0_u64;

        for operation in task.operations() {
            let claim_id = OperationClaimId::new(task.task_id(), operation_index);
            self.execute_operation_with_claims(
                working,
                prerequisite,
                operation,
                OperationExecutionContext {
                    claim_id,
                    expires_at: task.expires_at(),
                },
                claims,
            )?;
            operation_index = operation_index
                .checked_add(1)
                .ok_or(ConcurrentExecutionError::OperationIndexOverflow)?;
        }

        Ok(())
    }

    fn execute_operation_with_claims(
        &mut self,
        working: &mut BusinessState,
        prerequisite: &mut crate::state::PrerequisiteState,
        operation: &Operation,
        context: OperationExecutionContext,
        claims: &mut CurrencyClaimBook,
    ) -> Result<(), ConcurrentExecutionError> {
        match operation {
            Operation::Issue { account, count } => {
                self.issue(working, *account, *count)?;
            }
            Operation::Transfer {
                source,
                destination,
                amount,
            } => {
                let expires_at = context.expires_at;
                let transfer = self.establish_transfer_for_execution(
                    prerequisite,
                    context.claim_id.clone(),
                    *source,
                    *destination,
                    *amount,
                    expires_at,
                )?;
                let currencies = claims.claim_transfer_in_business_state(
                    working,
                    context.claim_id.clone(),
                    transfer.source_account,
                    *amount,
                )?;
                self.apply_established_transfer(
                    working,
                    prerequisite,
                    context.claim_id,
                    transfer,
                    &currencies,
                )?;
            }
            Operation::Destroy { currencies } => {
                self.validate_destroy_targets(working, currencies)?;
                claims.claim_explicit(context.claim_id, currencies)?;
                self.apply_destroy(working, currencies);
            }
            Operation::LeakRepair { leaked } => {
                let leaked_owners = self.validate_leaked_owners(working, leaked)?;
                let reserve = claims.claim_leak_repair_in_business_state(
                    context.claim_id,
                    working,
                    leaked,
                )?;
                self.require_unique_currency_list(&reserve)?;
                self.apply_leak_repair(working, leaked, leaked_owners, &reserve)?;
            }
        }

        Ok(())
    }

    fn execute_operation(
        &mut self,
        working: &mut BusinessState,
        prerequisite: &mut crate::state::PrerequisiteState,
        operation: &Operation,
        context: OperationExecutionContext,
    ) -> Result<(), ExecutionError> {
        match operation {
            Operation::Issue { account, count } => self.issue(working, *account, *count),
            Operation::Transfer {
                source,
                destination,
                amount,
            } => {
                let expires_at = context.expires_at;
                let transfer = self.establish_transfer_for_execution(
                    prerequisite,
                    context.claim_id.clone(),
                    *source,
                    *destination,
                    *amount,
                    expires_at,
                )?;
                let candidates =
                    self.select_transfer_candidates(working, transfer.source_account, *amount)?;
                self.apply_established_transfer(
                    working,
                    prerequisite,
                    context.claim_id,
                    transfer,
                    &candidates,
                )
            }
            Operation::Destroy { currencies } => self.destroy(working, currencies),
            Operation::LeakRepair { leaked } => self.leak_repair(working, leaked),
        }
    }

    fn issue(
        &mut self,
        working: &mut BusinessState,
        account: AccountAddress,
        count: u64,
    ) -> Result<(), ExecutionError> {
        self.require_account(working, account)?;
        let range = self.allocate_currency_range(count)?;
        self.apply_issue_preallocated(working, account, &range)
    }

    fn select_transfer_candidates(
        &self,
        working: &BusinessState,
        source: AccountAddress,
        amount: u64,
    ) -> Result<Vec<CurrencyAddress>, ExecutionError> {
        let mut candidates = Vec::new();

        for (address, currency) in &working.currencies {
            if currency.role == CurrencyRole::Circulation && currency.owner == Some(source) {
                candidates.push(*address);
                if candidates.len() as u64 == amount {
                    break;
                }
            }
        }

        if candidates.len() as u64 != amount {
            return Err(ExecutionError::InsufficientBalance {
                account: source,
                required: amount,
                available: candidates.len() as u64,
            });
        }

        Ok(candidates)
    }

    pub(crate) fn claim_transfer_candidates(
        &self,
        working: &mut BusinessState,
        source: AccountAddress,
        destination: AccountAddress,
        candidates: &[CurrencyAddress],
    ) -> Result<(), ExecutionError> {
        for address in candidates {
            let currency = working
                .currencies
                .get(address)
                .ok_or(ExecutionError::CurrencyNotFound(*address))?;

            if currency.role != CurrencyRole::Circulation {
                return Err(ExecutionError::CurrencyNotCirculation(*address));
            }
            if currency.owner != Some(source) {
                return Err(ExecutionError::CurrencyNotOwned(*address));
            }
        }

        for address in candidates {
            if let Some(currency) = working.currencies.get_mut(address) {
                currency.owner = Some(destination);
            }
        }

        Ok(())
    }

    fn destroy(
        &self,
        working: &mut BusinessState,
        currencies: &[CurrencyAddress],
    ) -> Result<(), ExecutionError> {
        self.validate_destroy_targets(working, currencies)?;
        self.apply_destroy(working, currencies);
        Ok(())
    }

    pub(crate) fn validate_destroy_targets(
        &self,
        working: &BusinessState,
        currencies: &[CurrencyAddress],
    ) -> Result<(), ExecutionError> {
        self.require_unique_currency_list(currencies)?;

        for address in currencies {
            let currency = working
                .currencies
                .get(address)
                .ok_or(ExecutionError::CurrencyNotFound(*address))?;

            if currency.role != CurrencyRole::Circulation {
                return Err(ExecutionError::CurrencyNotCirculation(*address));
            }
            if currency.owner.is_some() {
                return Err(ExecutionError::CurrencyStillOccupied(*address));
            }
        }

        Ok(())
    }

    pub(crate) fn apply_destroy(
        &self,
        working: &mut BusinessState,
        currencies: &[CurrencyAddress],
    ) {
        for address in currencies {
            working.currencies.remove(address);
        }
    }

    fn leak_repair(
        &mut self,
        working: &mut BusinessState,
        leaked: &[CurrencyAddress],
    ) -> Result<(), ExecutionError> {
        let leaked_owners = self.validate_leaked_owners(working, leaked)?;

        let reserve = working
            .currencies
            .iter()
            .filter_map(|(address, currency)| {
                (currency.role == CurrencyRole::Reserve && currency.owner.is_none())
                    .then_some(*address)
            })
            .take(leaked.len())
            .collect::<Vec<_>>();

        if reserve.len() != leaked.len() {
            return Err(ExecutionError::ReserveUnavailable {
                required: leaked.len() as u64,
                available: reserve.len() as u64,
            });
        }

        self.apply_leak_repair(working, leaked, leaked_owners, &reserve)
    }

    pub(crate) fn validate_leaked_owners(
        &self,
        working: &BusinessState,
        leaked: &[CurrencyAddress],
    ) -> Result<Vec<AccountAddress>, ExecutionError> {
        self.require_unique_currency_list(leaked)?;

        leaked
            .iter()
            .map(|address| {
                let currency = working
                    .currencies
                    .get(address)
                    .ok_or(ExecutionError::CurrencyNotFound(*address))?;

                if currency.role != CurrencyRole::Circulation {
                    return Err(ExecutionError::CurrencyNotCirculation(*address));
                }

                currency
                    .owner
                    .ok_or(ExecutionError::CurrencyNotOwned(*address))
            })
            .collect()
    }

    fn apply_leak_repair(
        &mut self,
        working: &mut BusinessState,
        leaked: &[CurrencyAddress],
        leaked_owners: Vec<AccountAddress>,
        reserve: &[CurrencyAddress],
    ) -> Result<(), ExecutionError> {
        let replacement_reserve = self.allocate_currency_range(leaked.len() as u64)?;
        self.apply_leak_repair_preallocated(
            working,
            leaked,
            &leaked_owners,
            reserve,
            &replacement_reserve,
        )
    }

    pub(crate) fn apply_leak_repair_preallocated(
        &self,
        working: &mut BusinessState,
        leaked: &[CurrencyAddress],
        leaked_owners: &[AccountAddress],
        reserve: &[CurrencyAddress],
        replacement_reserve: &[CurrencyAddress],
    ) -> Result<(), ExecutionError> {
        if leaked.len() != leaked_owners.len()
            || leaked.len() != reserve.len()
            || leaked.len() != replacement_reserve.len()
        {
            return Err(ExecutionError::ReserveUnavailable {
                required: leaked.len() as u64,
                available: reserve.len() as u64,
            });
        }

        self.require_unique_currency_list(leaked)?;
        self.require_unique_currency_list(reserve)?;
        self.require_unique_currency_list(replacement_reserve)?;

        for (address, expected_owner) in leaked.iter().zip(leaked_owners) {
            let currency = working
                .currencies
                .get(address)
                .ok_or(ExecutionError::CurrencyNotFound(*address))?;
            if currency.role != CurrencyRole::Circulation {
                return Err(ExecutionError::CurrencyNotCirculation(*address));
            }
            if currency.owner != Some(*expected_owner) {
                return Err(ExecutionError::CurrencyNotOwned(*address));
            }
        }

        for address in reserve {
            let currency = working
                .currencies
                .get(address)
                .ok_or(ExecutionError::CurrencyNotFound(*address))?;
            if currency.role != CurrencyRole::Reserve || currency.owner.is_some() {
                return Err(ExecutionError::CurrencyNotCirculation(*address));
            }
        }

        for address in replacement_reserve {
            if working.currencies.contains_key(address) {
                return Err(ExecutionError::DuplicateCurrency(*address));
            }
        }

        for ((leaked_address, reserve_address), owner) in
            leaked.iter().zip(reserve.iter()).zip(leaked_owners)
        {
            working.currencies.remove(leaked_address);

            let reserve_currency = working
                .currencies
                .get_mut(reserve_address)
                .ok_or(ExecutionError::CurrencyNotFound(*reserve_address))?;
            reserve_currency.role = CurrencyRole::Circulation;
            reserve_currency.owner = Some(*owner);
        }

        for address in replacement_reserve {
            working.currencies.insert(
                *address,
                Currency {
                    address: *address,
                    role: CurrencyRole::Reserve,
                    owner: None,
                },
            );
        }

        Ok(())
    }

    pub(crate) fn apply_issue_preallocated(
        &self,
        working: &mut BusinessState,
        account: AccountAddress,
        addresses: &[CurrencyAddress],
    ) -> Result<(), ExecutionError> {
        self.require_account(working, account)?;
        self.require_unique_currency_list(addresses)?;

        for address in addresses {
            if working.currencies.contains_key(address) {
                return Err(ExecutionError::DuplicateCurrency(*address));
            }
        }

        for address in addresses {
            working.currencies.insert(
                *address,
                Currency {
                    address: *address,
                    role: CurrencyRole::Circulation,
                    owner: Some(account),
                },
            );
        }

        Ok(())
    }

    pub(crate) fn require_account(
        &self,
        working: &BusinessState,
        account: AccountAddress,
    ) -> Result<(), ExecutionError> {
        if working.accounts.contains(&account) {
            Ok(())
        } else {
            Err(ExecutionError::AccountNotFound(account))
        }
    }

    pub(crate) fn require_unique_currency_list(
        &self,
        currencies: &[CurrencyAddress],
    ) -> Result<(), ExecutionError> {
        let mut seen = BTreeSet::new();
        for address in currencies {
            if !seen.insert(*address) {
                return Err(ExecutionError::DuplicateCurrency(*address));
            }
        }
        Ok(())
    }
}
