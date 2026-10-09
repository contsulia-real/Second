use crate::state::BusinessState;
use crate::{
    AccountAddress, ExecutionError, Operation, PaymentAddress, SecondState, VerifiedLegalTask,
};
use std::collections::BTreeMap;

impl SecondState {
    /// Preflight before resource allocation or a permanent TaskId binding.
    /// Operation replay validates the intermediate business state again.
    pub(crate) fn authorize_task(&self, task: &VerifiedLegalTask) -> Result<(), ExecutionError> {
        let mut registered = BTreeMap::<PaymentAddress, AccountAddress>::new();
        for operation in task.operations() {
            match operation {
                Operation::RegisterAccount { account } => {
                    task.require_account_signature(*account)?
                }
                Operation::RegisterPaymentAddress { address, account } => {
                    task.require_account_signature(*account)?;
                    registered.insert(*address, *account);
                }
                Operation::Transfer { source, .. }
                | Operation::RetirePaymentAddress { address: source }
                | Operation::FinalizePaymentAddressRetirement { address: source } => {
                    if let Some(account) = registered
                        .get(source)
                        .copied()
                        .or_else(|| self.payment_address_account(*source))
                    {
                        task.require_account_signature(account)?;
                    }
                }
                Operation::LeakRepair { leaked } => {
                    for address in leaked {
                        if let Some(owner) = self
                            .business
                            .currencies
                            .get(address)
                            .and_then(|currency| currency.owner)
                        {
                            task.require_account_signature(owner)?;
                        }
                    }
                }
                Operation::Issue { .. } | Operation::Destroy { .. } => {}
            }
        }
        Ok(())
    }

    pub fn has_account(&self, account: AccountAddress) -> bool {
        self.business.accounts.contains(&account)
    }

    pub(crate) fn register_account_in_business_state(
        &self,
        working: &mut BusinessState,
        account: AccountAddress,
    ) -> Result<(), ExecutionError> {
        if !working.accounts.insert(account) {
            return Err(ExecutionError::AccountAlreadyExists(account));
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
}
