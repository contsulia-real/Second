use sha2::{Digest, Sha256};

use crate::payment::EstablishedTransfer;
use crate::state::{BusinessState, PrerequisiteState};
use crate::{
    AccountAddress, ClaimError, CurrencyAddress, CurrencyClaimBook, OperationClaimId,
    PreparationError, SecondState, TaskId,
};

const PREPARED_TASK_DOMAIN: &[u8] = b"SECOND_PREPARED_TASK_V1\0";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PreparedOperation {
    Issue {
        account: AccountAddress,
        addresses: Vec<CurrencyAddress>,
    },
    Transfer {
        transfer: EstablishedTransfer,
        currencies: Vec<CurrencyAddress>,
    },
    Destroy {
        currencies: Vec<CurrencyAddress>,
    },
    LeakRepair {
        leaked: Vec<CurrencyAddress>,
        leaked_owners: Vec<AccountAddress>,
        reserve: Vec<CurrencyAddress>,
        replacement_reserve: Vec<CurrencyAddress>,
    },
}

impl PreparedOperation {
    pub(crate) fn apply(
        &self,
        state: &SecondState,
        working: &mut BusinessState,
        prerequisite: &mut PrerequisiteState,
        claim_id: OperationClaimId,
    ) -> Result<(), PreparationError> {
        match self {
            Self::Issue { account, addresses } => {
                state.apply_issue_preallocated(working, *account, addresses)?;
            }
            Self::Transfer {
                transfer,
                currencies,
            } => {
                state.apply_established_transfer(
                    working,
                    prerequisite,
                    claim_id,
                    *transfer,
                    currencies,
                )?;
            }
            Self::Destroy { currencies } => {
                state.validate_destroy_targets(working, currencies)?;
                state.apply_destroy(working, currencies);
            }
            Self::LeakRepair {
                leaked,
                leaked_owners,
                reserve,
                replacement_reserve,
            } => {
                state.apply_leak_repair_preallocated(
                    working,
                    leaked,
                    leaked_owners,
                    reserve,
                    replacement_reserve,
                )?;
            }
        }

        Ok(())
    }

    fn restore_claim(
        &self,
        claim_id: OperationClaimId,
        claims: &mut CurrencyClaimBook,
    ) -> Result<(), ClaimError> {
        match self {
            Self::Issue { .. } => Ok(()),
            Self::Transfer {
                transfer,
                currencies,
            } => claims.restore_transfer(claim_id, transfer.source_account, currencies),
            Self::Destroy { currencies } => claims.restore_explicit(claim_id, currencies),
            Self::LeakRepair {
                leaked, reserve, ..
            } => claims.restore_leak_repair(claim_id, leaked, reserve),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PreparedTask {
    pub(crate) task_id: TaskId,
    pub(crate) request_digest: [u8; 32],
    pub(crate) validator_set_version: u64,
    pub(crate) operations: Vec<PreparedOperation>,
}

impl PreparedTask {
    pub(crate) fn new(
        task_id: TaskId,
        request_digest: [u8; 32],
        validator_set_version: u64,
        operations: Vec<PreparedOperation>,
    ) -> Self {
        Self {
            task_id,
            request_digest,
            validator_set_version,
            operations,
        }
    }

    pub(crate) fn plan_digest(&self) -> Result<[u8; 32], PreparationError> {
        let mut hasher = Sha256::new();
        hasher.update(PREPARED_TASK_DOMAIN);
        hasher.update(self.request_digest);
        hash_len(&mut hasher, self.task_id.len())?;
        hasher.update(self.task_id.as_bytes());
        hasher.update(self.validator_set_version.to_be_bytes());
        hash_len(&mut hasher, self.operations.len())?;

        for operation in &self.operations {
            match operation {
                PreparedOperation::Issue { account, addresses } => {
                    hasher.update([1]);
                    hasher.update(account.bytes());
                    hash_addresses(&mut hasher, addresses)?;
                }
                PreparedOperation::Transfer {
                    transfer,
                    currencies,
                } => {
                    hasher.update([2]);
                    hasher.update(transfer.source.bytes());
                    hasher.update(transfer.destination.bytes());
                    hasher.update(transfer.source_account.bytes());
                    hasher.update(transfer.destination_account.bytes());
                    hasher.update(transfer.amount.to_be_bytes());
                    hash_addresses(&mut hasher, currencies)?;
                }
                PreparedOperation::Destroy { currencies } => {
                    hasher.update([3]);
                    hash_addresses(&mut hasher, currencies)?;
                }
                PreparedOperation::LeakRepair {
                    leaked,
                    leaked_owners,
                    reserve,
                    replacement_reserve,
                } => {
                    hasher.update([4]);
                    hash_addresses(&mut hasher, leaked)?;
                    hash_len(&mut hasher, leaked_owners.len())?;
                    for owner in leaked_owners {
                        hasher.update(owner.bytes());
                    }
                    hash_addresses(&mut hasher, reserve)?;
                    hash_addresses(&mut hasher, replacement_reserve)?;
                }
            }
        }

        Ok(hasher.finalize().into())
    }

    pub(crate) fn restore_claims(
        &self,
        claims: &mut CurrencyClaimBook,
    ) -> Result<(), PreparationError> {
        for (index, operation) in self.operations.iter().enumerate() {
            let operation_index =
                u64::try_from(index).map_err(|_| PreparationError::OperationIndexOverflow)?;
            operation.restore_claim(
                OperationClaimId::new(self.task_id.clone(), operation_index),
                claims,
            )?;
        }
        Ok(())
    }

    pub(crate) fn apply(
        &self,
        state: &mut SecondState,
        working: &mut BusinessState,
    ) -> Result<(), PreparationError> {
        let mut prerequisite = state.prerequisite.clone();

        for (index, operation) in self.operations.iter().enumerate() {
            let operation_index =
                u64::try_from(index).map_err(|_| PreparationError::OperationIndexOverflow)?;
            operation.apply(
                state,
                working,
                &mut prerequisite,
                OperationClaimId::new(self.task_id.clone(), operation_index),
            )?;
        }

        state.prerequisite = prerequisite;
        Ok(())
    }
}

fn hash_addresses(
    hasher: &mut Sha256,
    addresses: &[CurrencyAddress],
) -> Result<(), PreparationError> {
    hash_len(hasher, addresses.len())?;
    for address in addresses {
        hasher.update(address.value().to_be_bytes());
    }
    Ok(())
}

fn hash_len(hasher: &mut Sha256, len: usize) -> Result<(), PreparationError> {
    let len = u64::try_from(len).map_err(|_| PreparationError::LengthOverflow)?;
    hasher.update(len.to_be_bytes());
    Ok(())
}
