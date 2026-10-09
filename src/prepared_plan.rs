use std::collections::BTreeSet;

use sha2::{Digest, Sha256};

use crate::payment::EstablishedTransfer;
use crate::prepared::lifecycle::{LifecycleAddress, LifecycleClaimBook};
use crate::state::{BusinessState, PrerequisiteState};
use crate::{
    AccountAddress, CURRENT_PROTOCOL_VERSION, ClaimError, CurrencyAddress, CurrencyClaimBook,
    FinalityCertificate, FinalityStatement, LegalTask, OperationClaimId, PaymentAddress,
    PreparationError, SecondState, TaskId, ValidatorVote,
};

const PREPARED_TASK_DOMAIN: &[u8] = b"SECOND_PREPARED_TASK_V1\0";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PreparedOperation {
    RegisterAccount {
        account: AccountAddress,
    },
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
    RegisterPaymentAddress {
        address: PaymentAddress,
        account: AccountAddress,
    },
    RetirePaymentAddress {
        address: PaymentAddress,
    },
    FinalizePaymentAddressRetirement {
        address: PaymentAddress,
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
            Self::RegisterAccount { account } => {
                state.register_account_in_business_state(working, *account)?;
            }
            Self::Issue { account, addresses } => {
                validate_preallocated_addresses(state, addresses)?;
                state.apply_issue_preallocated(working, *account, addresses)?;
            }
            Self::Transfer {
                transfer,
                currencies,
            } => {
                let actual = u64::try_from(currencies.len())
                    .map_err(|_| PreparationError::LengthOverflow)?;
                if transfer.amount == 0 || actual != transfer.amount {
                    return Err(PreparationError::InvalidPreparedPlan);
                }
                state.require_unique_currency_list(currencies)?;
                state.apply_established_transfer(
                    working,
                    prerequisite,
                    claim_id,
                    *transfer,
                    currencies,
                )?;
            }
            Self::Destroy { currencies } => {
                if currencies.is_empty() {
                    return Err(PreparationError::InvalidPreparedPlan);
                }
                state.validate_destroy_targets(working, currencies)?;
                state.apply_destroy(working, currencies);
            }
            Self::LeakRepair {
                leaked,
                leaked_owners,
                reserve,
                replacement_reserve,
            } => {
                if leaked.is_empty()
                    || leaked.len() != leaked_owners.len()
                    || leaked.len() != reserve.len()
                    || leaked.len() != replacement_reserve.len()
                {
                    return Err(PreparationError::InvalidPreparedPlan);
                }
                validate_preallocated_addresses(state, replacement_reserve)?;
                state.apply_leak_repair_preallocated(
                    working,
                    leaked,
                    leaked_owners,
                    reserve,
                    replacement_reserve,
                )?;
            }
            Self::RegisterPaymentAddress { address, account } => {
                state.register_payment_address_in_business_state(working, *address, *account)?;
            }
            Self::RetirePaymentAddress { address } => {
                state.retire_payment_address_in_business_state(working, *address)?;
            }
            Self::FinalizePaymentAddressRetirement { address } => {
                state.finalize_payment_address_retirement_in_business_state(
                    working,
                    prerequisite,
                    *address,
                )?;
            }
        }

        Ok(())
    }

    pub(crate) fn restore_claim(
        &self,
        claim_id: OperationClaimId,
        claims: &mut CurrencyClaimBook,
    ) -> Result<(), ClaimError> {
        match self {
            Self::Issue { .. } | Self::RegisterAccount { .. } => Ok(()),
            Self::Transfer {
                transfer,
                currencies,
            } => claims.restore_transfer(claim_id, transfer.source_account, currencies),
            Self::Destroy { currencies } => claims.restore_explicit(claim_id, currencies),
            Self::LeakRepair {
                leaked, reserve, ..
            } => claims.restore_leak_repair(claim_id, leaked, reserve),
            Self::RegisterPaymentAddress { .. }
            | Self::RetirePaymentAddress { .. }
            | Self::FinalizePaymentAddressRetirement { .. } => Ok(()),
        }
    }

    pub(crate) fn lifecycle_address(&self) -> Option<LifecycleAddress> {
        match self {
            Self::RegisterAccount { account } => Some(LifecycleAddress::Account(*account)),
            Self::RegisterPaymentAddress { address, .. }
            | Self::RetirePaymentAddress { address }
            | Self::FinalizePaymentAddressRetirement { address } => {
                Some(LifecycleAddress::Payment(*address))
            }
            Self::Issue { .. }
            | Self::Transfer { .. }
            | Self::Destroy { .. }
            | Self::LeakRepair { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum PreparedTaskPhase {
    Prepared,
    Voting,
    Finalized,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PreparedTask {
    pub(crate) task_id: TaskId,
    pub(crate) request_digest: [u8; 32],
    pub(crate) source_task: LegalTask,
    pub(crate) validator_set_version: u64,
    pub(crate) phase: PreparedTaskPhase,
    pub(crate) commit_authorized: bool,
    pub(crate) conflict_abort: bool,
    pub(crate) finality_votes: Option<Vec<ValidatorVote>>,
    pub(crate) operations: Vec<PreparedOperation>,
    pub(crate) variants: Vec<PreparedVariant>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PreparedVariant {
    pub(crate) operations: Vec<PreparedOperation>,
    pub(crate) commit_authorized: bool,
}

impl PreparedTask {
    pub(crate) fn new(
        task_id: TaskId,
        request_digest: [u8; 32],
        source_task: LegalTask,
        validator_set_version: u64,
        operations: Vec<PreparedOperation>,
    ) -> Self {
        Self {
            task_id,
            request_digest,
            source_task,
            validator_set_version,
            phase: PreparedTaskPhase::Prepared,
            commit_authorized: true,
            conflict_abort: false,
            finality_votes: None,
            operations,
            variants: Vec::new(),
        }
    }

    pub(crate) fn from_persisted(
        task_id: TaskId,
        request_digest: [u8; 32],
        source_task: LegalTask,
        validator_set_version: u64,
        phase: PreparedTaskPhase,
        finality_votes: Option<Vec<ValidatorVote>>,
        operations: Vec<PreparedOperation>,
    ) -> Self {
        Self {
            task_id,
            request_digest,
            source_task,
            validator_set_version,
            phase,
            commit_authorized: true,
            conflict_abort: false,
            finality_votes,
            operations,
            variants: Vec::new(),
        }
    }

    pub(crate) fn finality_certificate(
        &self,
    ) -> Result<Option<FinalityCertificate>, PreparationError> {
        let Some(votes) = &self.finality_votes else {
            return Ok(None);
        };
        Ok(Some(FinalityCertificate::from_untrusted_parts(
            FinalityStatement::new(
                CURRENT_PROTOCOL_VERSION,
                self.validator_set_version,
                self.plan_digest()?,
            ),
            votes.clone(),
        )))
    }

    pub(crate) fn public_currency_change_addresses(&self) -> BTreeSet<CurrencyAddress> {
        let mut addresses = BTreeSet::new();
        for operation in &self.operations {
            match operation {
                PreparedOperation::Issue {
                    addresses: issued, ..
                } => addresses.extend(issued.iter().copied()),
                PreparedOperation::Transfer { currencies, .. }
                | PreparedOperation::Destroy { currencies } => {
                    addresses.extend(currencies.iter().copied());
                }
                PreparedOperation::LeakRepair {
                    leaked,
                    reserve,
                    replacement_reserve,
                    ..
                } => {
                    addresses.extend(leaked.iter().copied());
                    addresses.extend(reserve.iter().copied());
                    addresses.extend(replacement_reserve.iter().copied());
                }
                PreparedOperation::RegisterPaymentAddress { .. }
                | PreparedOperation::RegisterAccount { .. }
                | PreparedOperation::RetirePaymentAddress { .. }
                | PreparedOperation::FinalizePaymentAddressRetirement { .. } => {}
            }
        }
        addresses
    }

    pub(crate) fn finalize_with_votes(&mut self, votes: Vec<ValidatorVote>) {
        self.phase = PreparedTaskPhase::Finalized;
        self.finality_votes = Some(votes);
    }

    pub(crate) fn advance_phase(&mut self, phase: PreparedTaskPhase) -> bool {
        if phase <= self.phase {
            return false;
        }

        self.phase = phase;
        true
    }

    pub(crate) fn plan_digest(&self) -> Result<[u8; 32], PreparationError> {
        self.operations_digest(&self.operations)
    }

    pub(crate) fn operations_digest(
        &self,
        operations: &[PreparedOperation],
    ) -> Result<[u8; 32], PreparationError> {
        let mut hasher = Sha256::new();
        hasher.update(PREPARED_TASK_DOMAIN);
        hasher.update(self.request_digest);
        hash_len(&mut hasher, self.task_id.len())?;
        hasher.update(self.task_id.as_bytes());
        hasher.update(self.validator_set_version.to_be_bytes());
        hash_len(&mut hasher, operations.len())?;

        for operation in operations {
            match operation {
                PreparedOperation::RegisterAccount { account } => {
                    hasher.update([8]);
                    hasher.update(account.bytes());
                }
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
                PreparedOperation::RegisterPaymentAddress { address, account } => {
                    hasher.update([5]);
                    hasher.update(address.bytes());
                    hasher.update(account.bytes());
                }
                PreparedOperation::RetirePaymentAddress { address } => {
                    hasher.update([6]);
                    hasher.update(address.bytes());
                }
                PreparedOperation::FinalizePaymentAddressRetirement { address } => {
                    hasher.update([7]);
                    hasher.update(address.bytes());
                }
            }
        }

        Ok(hasher.finalize().into())
    }

    pub(crate) fn restore_claims(
        &self,
        claims: &mut CurrencyClaimBook,
    ) -> Result<(), PreparationError> {
        self.restore_candidate_claims(claims, false)
    }

    pub(crate) fn restore_lifecycle_claims(
        &self,
        claims: &mut LifecycleClaimBook,
    ) -> Result<(), PreparationError> {
        self.restore_candidate_lifecycle_claims(claims, false)
    }

    pub(crate) fn apply(
        &self,
        state: &mut SecondState,
        working: &mut BusinessState,
    ) -> Result<(), PreparationError> {
        self.apply_inner(state, working, false)
    }

    pub(crate) fn apply_certified(
        &self,
        state: &mut SecondState,
        working: &mut BusinessState,
    ) -> Result<(), PreparationError> {
        self.apply_inner(
            state,
            working,
            self.phase == PreparedTaskPhase::Finalized && !self.commit_authorized,
        )
    }

    fn apply_inner(
        &self,
        state: &mut SecondState,
        working: &mut BusinessState,
        establish_missing: bool,
    ) -> Result<(), PreparationError> {
        let mut prerequisite = state.prerequisite.clone();
        let inherited_establishment = if establish_missing {
            state
                .protocol
                .task_handoff
                .as_ref()
                .map(|handoff| {
                    self.plan_digest()
                        .map(|digest| handoff.plans.contains_key(&(self.task_id.clone(), digest)))
                })
                .transpose()?
                .unwrap_or(false)
        } else {
            false
        };

        for (index, operation) in self.operations.iter().enumerate() {
            let operation_index =
                u64::try_from(index).map_err(|_| PreparationError::OperationIndexOverflow)?;
            let claim_id = OperationClaimId::new(self.task_id.clone(), operation_index);
            if establish_missing
                && !prerequisite.payment_executions.contains_key(&claim_id)
                && let PreparedOperation::Transfer { transfer, .. } = operation
            {
                if inherited_establishment {
                    state.restore_certified_transfer_in_prerequisite(
                        working,
                        &mut prerequisite,
                        claim_id.clone(),
                        *transfer,
                    )?;
                } else {
                    let actual = state.establish_transfer_in_prerequisite(
                        working,
                        &mut prerequisite,
                        claim_id.clone(),
                        transfer.source,
                        transfer.destination,
                        transfer.amount,
                    )?;
                    if actual != *transfer {
                        return Err(PreparationError::InvalidPreparedPlan);
                    }
                }
            }
            operation.apply(state, working, &mut prerequisite, claim_id)?;
        }

        state.prerequisite = prerequisite;
        Ok(())
    }
}

fn validate_preallocated_addresses(
    state: &SecondState,
    addresses: &[CurrencyAddress],
) -> Result<(), PreparationError> {
    if addresses.is_empty()
        || addresses
            .iter()
            .any(|address| address.value() >= state.next_currency_address())
    {
        return Err(PreparationError::InvalidPreparedPlan);
    }

    Ok(())
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
