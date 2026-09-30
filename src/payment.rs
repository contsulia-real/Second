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
        let status = self
            .business
            .payment_addresses
            .get(&address)
            .map(|record| record.status)
            .ok_or(ExecutionError::PaymentAddressUnavailable(address))?;

        if status != PaymentAddressStatus::Retiring {
            return Err(ExecutionError::InvalidPaymentAddressTransition(address));
        }

        if self
            .prerequisite
            .payment_executions
            .values()
            .any(|execution| execution.source == address || execution.destination == address)
        {
            return Err(ExecutionError::InvalidPaymentAddressTransition(address));
        }

        self.business
            .payment_addresses
            .get_mut(&address)
            .ok_or(ExecutionError::PaymentAddressUnavailable(address))?
            .status = PaymentAddressStatus::Retired;
        Ok(())
    }

    pub(crate) fn establish_transfer(
        &mut self,
        claim_id: OperationClaimId,
        source: PaymentAddress,
        destination: PaymentAddress,
        amount: u64,
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

        let source_account = self.require_payment_address_available_for_establishment(source)?;
        let destination_account =
            self.require_payment_address_available_for_establishment(destination)?;

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

    pub(crate) fn establish_transfer_for_execution(
        &mut self,
        working_prerequisite: &mut PrerequisiteState,
        claim_id: OperationClaimId,
        source: PaymentAddress,
        destination: PaymentAddress,
        amount: u64,
        expires_at: u64,
    ) -> Result<EstablishedTransfer, ExecutionError> {
        let transfer =
            self.establish_transfer(claim_id.clone(), source, destination, amount, expires_at)?;
        let execution = self
            .prerequisite
            .payment_executions
            .get(&claim_id)
            .cloned()
            .ok_or(ExecutionError::TransferNotEstablished(claim_id.clone()))?;
        working_prerequisite
            .payment_executions
            .insert(claim_id, execution);
        Ok(transfer)
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
    ) -> Result<AccountAddress, ExecutionError> {
        let record = self
            .business
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
