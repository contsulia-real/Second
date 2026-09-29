use std::collections::BTreeSet;

use crate::currency::Currency;
use crate::state::{BusinessState, ExecutionOutcome, SecondState, TaskBinding};
use crate::{
    AccountAddress, CurrencyAddress, CurrencyRole, ExecutionError, Operation, VerifiedLegalTask,
};

impl SecondState {
    pub fn execute(
        &mut self,
        task: &VerifiedLegalTask,
        now: u64,
    ) -> Result<ExecutionOutcome, ExecutionError> {
        match self.protocol.task_bindings.get(&task.task_id()) {
            Some(binding) if binding.request_digest != task.request_digest() => {
                return Err(ExecutionError::TaskIdAlreadyBound);
            }
            Some(binding) if binding.succeeded => {
                return Ok(ExecutionOutcome::AlreadySucceeded);
            }
            Some(_) => {}
            None => {
                self.protocol.task_bindings.insert(
                    task.task_id(),
                    TaskBinding {
                        request_digest: task.request_digest(),
                        succeeded: false,
                    },
                );
            }
        }

        if task.expires_at().is_some_and(|expires_at| now > expires_at) {
            return Err(ExecutionError::TaskExpired);
        }

        let mut working = self.business.clone();

        for operation in task.operations() {
            self.execute_operation(&mut working, operation)?;
        }

        self.business = working;
        if let Some(binding) = self.protocol.task_bindings.get_mut(&task.task_id()) {
            binding.succeeded = true;
        }

        Ok(ExecutionOutcome::Succeeded)
    }

    fn execute_operation(
        &mut self,
        working: &mut BusinessState,
        operation: &Operation,
    ) -> Result<(), ExecutionError> {
        match operation {
            Operation::Issue { account, count } => self.issue(working, *account, *count),
            Operation::Transfer {
                source,
                destination,
                amount,
            } => self.transfer(working, *source, *destination, *amount),
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

        for address in range {
            working.currencies.insert(
                address,
                Currency {
                    address,
                    role: CurrencyRole::Circulation,
                    owner: Some(account),
                },
            );
        }

        Ok(())
    }

    fn transfer(
        &mut self,
        working: &mut BusinessState,
        source: AccountAddress,
        destination: AccountAddress,
        amount: u64,
    ) -> Result<(), ExecutionError> {
        self.require_account(working, source)?;
        self.require_account(working, destination)?;

        let candidates = self.select_transfer_candidates(working, source, amount)?;
        self.claim_transfer_candidates(working, source, destination, &candidates)
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

    fn claim_transfer_candidates(
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

        for address in currencies {
            working.currencies.remove(address);
        }

        Ok(())
    }

    fn leak_repair(
        &mut self,
        working: &mut BusinessState,
        leaked: &[CurrencyAddress],
    ) -> Result<(), ExecutionError> {
        self.require_unique_currency_list(leaked)?;

        let leaked_owners = leaked
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
            .collect::<Result<Vec<_>, _>>()?;

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

        let replacement_reserve = self.allocate_currency_range(leaked.len() as u64)?;

        for ((leaked_address, reserve_address), owner) in
            leaked.iter().zip(reserve.iter()).zip(leaked_owners)
        {
            working.currencies.remove(leaked_address);

            let reserve_currency = working
                .currencies
                .get_mut(reserve_address)
                .ok_or(ExecutionError::CurrencyNotFound(*reserve_address))?;
            reserve_currency.role = CurrencyRole::Circulation;
            reserve_currency.owner = Some(owner);
        }

        for address in replacement_reserve {
            working.currencies.insert(
                address,
                Currency {
                    address,
                    role: CurrencyRole::Reserve,
                    owner: None,
                },
            );
        }

        Ok(())
    }

    fn require_account(
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

    fn require_unique_currency_list(
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
