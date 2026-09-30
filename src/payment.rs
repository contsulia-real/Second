use crate::state::{BusinessState, PrerequisiteState};
use crate::{AccountAddress, ExecutionError, OperationClaimId, PaymentAddress, SecondState};

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
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PaymentExecution {
    pub(crate) source: PaymentAddress,
    pub(crate) destination: PaymentAddress,
    pub(crate) amount: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct EstablishedTransfer {
    pub(crate) source: PaymentAddress,
    pub(crate) destination: PaymentAddress,
    pub(crate) source_account: AccountAddress,
    pub(crate) destination_account: AccountAddress,
    pub(crate) amount: u64,
}

impl PaymentExecution {
    pub(crate) fn matches(
        &self,
        source: PaymentAddress,
        destination: PaymentAddress,
        amount: u64,
    ) -> bool {
        self.source == source && self.destination == destination && self.amount == amount
    }
}

impl SecondState {
    pub(crate) fn register_payment_address_in_business_state(
        &self,
        working: &mut BusinessState,
        address: PaymentAddress,
        account: AccountAddress,
    ) -> Result<(), ExecutionError> {
        if !working.accounts.contains(&account) {
            return Err(ExecutionError::AccountNotFound(account));
        }
        if working.payment_addresses.contains_key(&address) {
            return Err(ExecutionError::PaymentAddressAlreadyExists(address));
        }

        working.payment_addresses.insert(
            address,
            PaymentAddressRecord {
                account,
                status: PaymentAddressStatus::Active,
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

    pub fn payment_execution_count(&self) -> usize {
        self.prerequisite.payment_executions.len()
    }

    pub(crate) fn retire_payment_address_in_business_state(
        &self,
        working: &mut BusinessState,
        address: PaymentAddress,
    ) -> Result<(), ExecutionError> {
        let record = working
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

    pub(crate) fn finalize_payment_address_retirement_in_business_state(
        &self,
        working: &mut BusinessState,
        prerequisite: &PrerequisiteState,
        address: PaymentAddress,
    ) -> Result<(), ExecutionError> {
        let status = working
            .payment_addresses
            .get(&address)
            .map(|record| record.status)
            .ok_or(ExecutionError::PaymentAddressUnavailable(address))?;

        if status != PaymentAddressStatus::Retiring {
            return Err(ExecutionError::InvalidPaymentAddressTransition(address));
        }

        if prerequisite
            .payment_executions
            .values()
            .any(|execution| execution.source == address || execution.destination == address)
        {
            return Err(ExecutionError::InvalidPaymentAddressTransition(address));
        }

        working
            .payment_addresses
            .get_mut(&address)
            .ok_or(ExecutionError::PaymentAddressUnavailable(address))?
            .status = PaymentAddressStatus::Retired;
        Ok(())
    }

    pub(crate) fn establish_transfer(
        &mut self,
        working: &BusinessState,
        claim_id: OperationClaimId,
        source: PaymentAddress,
        destination: PaymentAddress,
        amount: u64,
    ) -> Result<EstablishedTransfer, ExecutionError> {
        if let Some(existing) = self.prerequisite.payment_executions.get(&claim_id) {
            if !existing.matches(source, destination, amount) {
                return Err(ExecutionError::InFlightTransferMismatch(claim_id));
            }

            return Ok(EstablishedTransfer {
                source,
                destination,
                source_account: self.payment_account_in_business_state(working, source)?,
                destination_account: self
                    .payment_account_in_business_state(working, destination)?,
                amount,
            });
        }

        let source_account =
            self.require_payment_address_available_for_establishment(working, source)?;
        let destination_account =
            self.require_payment_address_available_for_establishment(working, destination)?;

        self.prerequisite.payment_executions.insert(
            claim_id,
            PaymentExecution {
                source,
                destination,
                amount,
            },
        );

        Ok(EstablishedTransfer {
            source,
            destination,
            source_account,
            destination_account,
            amount,
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

        if !execution.matches(transfer.source, transfer.destination, transfer.amount) {
            return Err(ExecutionError::InFlightTransferMismatch(claim_id));
        }

        self.validate_established_transfer_for_execution(working, transfer)?;
        self.claim_transfer_candidates(
            working,
            transfer.source_account,
            transfer.destination_account,
            currencies,
        )?;
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

    fn payment_account_in_business_state(
        &self,
        working: &BusinessState,
        address: PaymentAddress,
    ) -> Result<AccountAddress, ExecutionError> {
        working
            .payment_addresses
            .get(&address)
            .map(|record| record.account)
            .ok_or(ExecutionError::PaymentAddressUnavailable(address))
    }

    fn require_payment_address_available_for_establishment(
        &self,
        working: &BusinessState,
        address: PaymentAddress,
    ) -> Result<AccountAddress, ExecutionError> {
        let record = working
            .payment_addresses
            .get(&address)
            .ok_or(ExecutionError::PaymentAddressUnavailable(address))?;

        if record.status != PaymentAddressStatus::Active {
            return Err(ExecutionError::PaymentAddressUnavailable(address));
        }

        Ok(record.account)
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
