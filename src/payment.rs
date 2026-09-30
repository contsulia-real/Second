use crate::state::{BusinessState, PrerequisiteState};
use crate::{
    AccountAddress, ExecutionError, Operation, OperationClaimId, PaymentAddress, SecondState,
    VerifiedLegalTask,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PaymentAddressStatus {
    Active,
    Retiring,
    Retired,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PaymentAddressRecord {
    pub(crate) account: AccountAddress,
    pub(crate) status: PaymentAddressStatus,
    pub(crate) usage_count: u64,
    pub(crate) expires_at: u64,
    pub(crate) max_usage: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PaymentExecution {
    pub(crate) source: PaymentAddress,
    pub(crate) destination: PaymentAddress,
    pub(crate) amount: u64,
    pub(crate) expires_at: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct EstablishedTransfer {
    pub(crate) source: PaymentAddress,
    pub(crate) destination: PaymentAddress,
    pub(crate) source_account: AccountAddress,
    pub(crate) destination_account: AccountAddress,
    pub(crate) amount: u64,
    pub(crate) expires_at: u64,
}

impl PaymentExecution {
    pub(crate) fn matches(
        &self,
        source: PaymentAddress,
        destination: PaymentAddress,
        amount: u64,
        expires_at: u64,
    ) -> bool {
        self.source == source
            && self.destination == destination
            && self.amount == amount
            && self.expires_at == expires_at
    }
}

impl SecondState {
    pub fn register_payment_address(
        &mut self,
        address: PaymentAddress,
        account: AccountAddress,
        expires_at: u64,
        max_usage: u64,
    ) -> Result<(), ExecutionError> {
        if !self.business.accounts.contains(&account) {
            return Err(ExecutionError::AccountNotFound(account));
        }
        if self.business.payment_addresses.contains_key(&address) {
            return Err(ExecutionError::PaymentAddressAlreadyExists(address));
        }

        self.business.payment_addresses.insert(
            address,
            PaymentAddressRecord {
                account,
                status: PaymentAddressStatus::Active,
                usage_count: 0,
                expires_at,
                max_usage,
            },
        );
        Ok(())
    }

    pub fn payment_address_status(&self, address: PaymentAddress) -> Option<PaymentAddressStatus> {
        self.business
            .payment_addresses
            .get(&address)
            .map(|record| record.status)
    }

    pub fn payment_address_account(&self, address: PaymentAddress) -> Option<AccountAddress> {
        self.business
            .payment_addresses
            .get(&address)
            .map(|record| record.account)
    }

    pub fn payment_address_usage_count(&self, address: PaymentAddress) -> Option<u64> {
        self.business
            .payment_addresses
            .get(&address)
            .map(|record| record.usage_count)
    }

    pub fn payment_execution_count(&self) -> usize {
        self.prerequisite.payment_executions.len()
    }

    pub fn reap_expired_payment_executions(&mut self, now: u64, max_count: usize) -> usize {
        let mut expired = self
            .prerequisite
            .payment_executions
            .iter()
            .filter(|(_, execution)| execution.expires_at <= now)
            .map(|(claim_id, execution)| (execution.expires_at, claim_id.clone()))
            .collect::<Vec<_>>();
        expired.sort_unstable();

        let removed = expired.len().min(max_count);
        for (_, claim_id) in expired.into_iter().take(max_count) {
            self.prerequisite.payment_executions.remove(&claim_id);
        }
        removed
    }

    pub fn retire_payment_address(
        &mut self,
        address: PaymentAddress,
    ) -> Result<(), ExecutionError> {
        let record = self
            .business
            .payment_addresses
            .get_mut(&address)
            .ok_or(ExecutionError::PaymentAddressUnavailable(address))?;

        match record.status {
            PaymentAddressStatus::Active => {
                record.status = PaymentAddressStatus::Retiring;
                Ok(())
            }
            PaymentAddressStatus::Retiring | PaymentAddressStatus::Retired => {
                Err(ExecutionError::InvalidPaymentAddressTransition(address))
            }
        }
    }

    pub fn finalize_payment_address_retirement(
        &mut self,
        address: PaymentAddress,
    ) -> Result<(), ExecutionError> {
        let record = self
            .business
            .payment_addresses
            .get_mut(&address)
            .ok_or(ExecutionError::PaymentAddressUnavailable(address))?;

        match record.status {
            PaymentAddressStatus::Retiring => {
                record.status = PaymentAddressStatus::Retired;
                Ok(())
            }
            PaymentAddressStatus::Active | PaymentAddressStatus::Retired => {
                Err(ExecutionError::InvalidPaymentAddressTransition(address))
            }
        }
    }

    pub(crate) fn establish_task_transfers(
        &mut self,
        task: &VerifiedLegalTask,
        now: u64,
    ) -> Result<(), ExecutionError> {
        let mut candidate = self.clone();

        for (index, operation) in task.operations().iter().enumerate() {
            let Operation::Transfer {
                source,
                destination,
                amount,
            } = operation
            else {
                continue;
            };
            let operation_index =
                u64::try_from(index).map_err(|_| ExecutionError::OperationIndexOverflow)?;
            let expires_at = task.expires_at();

            candidate.establish_transfer(
                OperationClaimId::new(task.task_id(), operation_index),
                *source,
                *destination,
                *amount,
                now,
                expires_at,
            )?;
        }

        self.prerequisite = candidate.prerequisite;
        Ok(())
    }

    pub(crate) fn establish_transfer(
        &mut self,
        claim_id: OperationClaimId,
        source: PaymentAddress,
        destination: PaymentAddress,
        amount: u64,
        now: u64,
        expires_at: u64,
    ) -> Result<EstablishedTransfer, ExecutionError> {
        if let Some(existing) = self.prerequisite.payment_executions.get(&claim_id) {
            if !existing.matches(source, destination, amount, expires_at) {
                return Err(ExecutionError::InFlightTransferMismatch(claim_id));
            }

            return Ok(EstablishedTransfer {
                source,
                destination,
                source_account: self.payment_account(source)?,
                destination_account: self.payment_account(destination)?,
                amount,
                expires_at,
            });
        }

        let source_account =
            self.require_payment_address_available_for_establishment(source, now)?;
        let destination_account =
            self.require_payment_address_available_for_establishment(destination, now)?;

        self.require_payment_capacity(source)?;
        if destination != source {
            self.require_payment_capacity(destination)?;
        }

        self.prerequisite.payment_executions.insert(
            claim_id,
            PaymentExecution {
                source,
                destination,
                amount,
                expires_at,
            },
        );

        Ok(EstablishedTransfer {
            source,
            destination,
            source_account,
            destination_account,
            amount,
            expires_at,
        })
    }

    pub(crate) fn apply_established_transfer(
        &self,
        working: &mut BusinessState,
        prerequisite: &mut PrerequisiteState,
        claim_id: OperationClaimId,
        transfer: EstablishedTransfer,
        currencies: &[crate::CurrencyAddress],
    ) -> Result<(), ExecutionError> {
        let execution = prerequisite
            .payment_executions
            .get(&claim_id)
            .ok_or(ExecutionError::TransferNotEstablished(claim_id.clone()))?;

        if !execution.matches(
            transfer.source,
            transfer.destination,
            transfer.amount,
            transfer.expires_at,
        ) {
            return Err(ExecutionError::InFlightTransferMismatch(claim_id));
        }

        self.validate_established_transfer_for_execution(working, transfer)?;
        self.claim_transfer_candidates(
            working,
            transfer.source_account,
            transfer.destination_account,
            currencies,
        )?;
        increment_usage(working, transfer.source)?;
        if transfer.destination != transfer.source {
            increment_usage(working, transfer.destination)?;
        }
        prerequisite.payment_executions.remove(&claim_id);
        Ok(())
    }

    pub(crate) fn validate_established_transfer_for_execution(
        &self,
        working: &BusinessState,
        transfer: EstablishedTransfer,
    ) -> Result<(), ExecutionError> {
        self.require_payment_address_for_execution(
            working,
            transfer.source,
            transfer.source_account,
        )?;
        self.require_payment_address_for_execution(
            working,
            transfer.destination,
            transfer.destination_account,
        )
    }

    fn payment_account(&self, address: PaymentAddress) -> Result<AccountAddress, ExecutionError> {
        self.business
            .payment_addresses
            .get(&address)
            .map(|record| record.account)
            .ok_or(ExecutionError::PaymentAddressUnavailable(address))
    }

    fn require_payment_address_available_for_establishment(
        &self,
        address: PaymentAddress,
        now: u64,
    ) -> Result<AccountAddress, ExecutionError> {
        let record = self
            .business
            .payment_addresses
            .get(&address)
            .ok_or(ExecutionError::PaymentAddressUnavailable(address))?;

        if record.status != PaymentAddressStatus::Active || record.expires_at <= now {
            return Err(ExecutionError::PaymentAddressUnavailable(address));
        }

        Ok(record.account)
    }

    fn require_payment_capacity(&self, address: PaymentAddress) -> Result<(), ExecutionError> {
        let record = self
            .business
            .payment_addresses
            .get(&address)
            .ok_or(ExecutionError::PaymentAddressUnavailable(address))?;
        let in_flight = self
            .prerequisite
            .payment_executions
            .values()
            .filter(|execution| execution.source == address || execution.destination == address)
            .count() as u64;
        let reserved = record
            .usage_count
            .checked_add(in_flight)
            .and_then(|value| value.checked_add(1))
            .ok_or(ExecutionError::PaymentAddressUsageOverflow(address))?;

        if reserved > record.max_usage {
            return Err(ExecutionError::PaymentAddressUsageExhausted(address));
        }

        Ok(())
    }

    fn require_payment_address_for_execution(
        &self,
        working: &BusinessState,
        address: PaymentAddress,
        expected_account: AccountAddress,
    ) -> Result<(), ExecutionError> {
        let record = working
            .payment_addresses
            .get(&address)
            .ok_or(ExecutionError::PaymentAddressUnavailable(address))?;

        if record.account != expected_account || record.status == PaymentAddressStatus::Retired {
            return Err(ExecutionError::PaymentAddressUnavailable(address));
        }

        Ok(())
    }
}

fn increment_usage(
    working: &mut BusinessState,
    address: PaymentAddress,
) -> Result<(), ExecutionError> {
    let record = working
        .payment_addresses
        .get_mut(&address)
        .ok_or(ExecutionError::PaymentAddressUnavailable(address))?;
    record.usage_count = record
        .usage_count
        .checked_add(1)
        .ok_or(ExecutionError::PaymentAddressUsageOverflow(address))?;
    Ok(())
}
