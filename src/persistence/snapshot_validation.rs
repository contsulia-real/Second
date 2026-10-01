use std::collections::{BTreeMap, BTreeSet};

use crate::ConsensusScope;
use crate::payment::{PaymentAddressRecord, PaymentAddressStatus, PaymentExecution};
use crate::prepared_plan::{PreparedOperation, PreparedTask, PreparedTaskPhase};
use crate::state::TaskBinding;
use crate::{
    BftLocalState, CurrencyClaimBook, OperationClaimId, PaymentAddress, PersistenceError,
    SecondState, TaskId, ValidatorId, ValidatorRegistry, ValidatorSet,
};

pub(super) fn resolve_validator_set<'a>(
    active_validator_set: &'a ValidatorSet,
    retained_validator_sets: &'a BTreeMap<u64, ValidatorSet>,
    version: u64,
) -> Option<&'a ValidatorSet> {
    if active_validator_set.version() == version {
        Some(active_validator_set)
    } else {
        retained_validator_sets.get(&version)
    }
}

pub(super) fn validate_active_prepared_vote_lock_membership(
    active_validator_set: &ValidatorSet,
    retained_validator_sets: &BTreeMap<u64, ValidatorSet>,
    prepared_tasks: &BTreeMap<TaskId, PreparedTask>,
    validator_vote_locks: &BTreeMap<(ValidatorId, ConsensusScope), [u8; 32]>,
) -> Result<(), PersistenceError> {
    for (validator_id, scope) in validator_vote_locks.keys() {
        let ConsensusScope::PreparedTask(task_id) = scope else {
            continue;
        };
        let Some(prepared) = prepared_tasks.get(task_id) else {
            continue;
        };
        let validator_set = resolve_validator_set(
            active_validator_set,
            retained_validator_sets,
            prepared.validator_set_version,
        )
        .ok_or(PersistenceError::InvalidSnapshot)?;
        if !validator_set.contains(*validator_id) {
            return Err(PersistenceError::InvalidSnapshot);
        }
    }

    Ok(())
}

pub(super) fn validate_retained_validator_sets(
    active_validator_set: &ValidatorSet,
    retained_validator_sets: &BTreeMap<u64, ValidatorSet>,
    validator_registry: &ValidatorRegistry,
    prepared_tasks: &BTreeMap<TaskId, PreparedTask>,
) -> Result<(), PersistenceError> {
    let referenced_versions = prepared_tasks
        .values()
        .map(|prepared| prepared.validator_set_version)
        .collect::<BTreeSet<_>>();

    for (version, retained) in retained_validator_sets {
        if *version != retained.version()
            || *version >= active_validator_set.version()
            || !referenced_versions.contains(version)
        {
            return Err(PersistenceError::InvalidSnapshot);
        }

        validator_registry
            .validate_historical_set(retained)
            .map_err(|_| PersistenceError::InvalidSnapshot)?;
    }

    for version in referenced_versions {
        if version == active_validator_set.version() {
            continue;
        }
        if !retained_validator_sets.contains_key(&version) {
            return Err(PersistenceError::InvalidSnapshot);
        }
    }

    Ok(())
}

pub(super) fn validate_vote_lock_registry(
    validator_registry: &ValidatorRegistry,
    validator_vote_locks: &BTreeMap<(ValidatorId, ConsensusScope), [u8; 32]>,
) -> Result<(), PersistenceError> {
    if validator_vote_locks
        .keys()
        .any(|(validator_id, _)| !validator_registry.contains(*validator_id))
    {
        return Err(PersistenceError::InvalidSnapshot);
    }

    Ok(())
}

pub(super) fn validate_bft_local_state_registry(
    validator_registry: &ValidatorRegistry,
    active_validator_set: &ValidatorSet,
    retained_validator_sets: &BTreeMap<u64, ValidatorSet>,
    prepared_tasks: &BTreeMap<TaskId, PreparedTask>,
    bft_local_states: &BTreeMap<(ValidatorId, ConsensusScope), BftLocalState>,
) -> Result<(), PersistenceError> {
    for ((validator_id, scope), state) in bft_local_states {
        if !validator_registry.contains(*validator_id) {
            return Err(PersistenceError::InvalidSnapshot);
        }

        match scope {
            ConsensusScope::PreparedTask(task_id) => {
                let prepared = prepared_tasks
                    .get(task_id)
                    .ok_or(PersistenceError::InvalidSnapshot)?;
                if prepared.validator_set_version != state.validator_set_version() {
                    return Err(PersistenceError::InvalidSnapshot);
                }
                let validator_set = resolve_validator_set(
                    active_validator_set,
                    retained_validator_sets,
                    state.validator_set_version(),
                )
                .ok_or(PersistenceError::InvalidSnapshot)?;
                if !validator_set.contains(*validator_id) {
                    return Err(PersistenceError::InvalidSnapshot);
                }
            }
            ConsensusScope::PublicCheckpoint {
                validator_set_version,
                ..
            }
            | ConsensusScope::StateRecoveryCheckpoint {
                validator_set_version,
                ..
            } => {
                if *validator_set_version != active_validator_set.version()
                    || state.validator_set_version() != active_validator_set.version()
                    || !active_validator_set.contains(*validator_id)
                {
                    return Err(PersistenceError::InvalidSnapshot);
                }
            }
            ConsensusScope::ValidatorSetTransition {
                current_validator_set_version,
            } => {
                if *current_validator_set_version != active_validator_set.version()
                    || state.validator_set_version() != active_validator_set.version()
                    || !active_validator_set.contains(*validator_id)
                {
                    return Err(PersistenceError::InvalidSnapshot);
                }
            }
        }
    }

    Ok(())
}

pub(super) fn validate_prepared_plans_against_state(
    state: &SecondState,
    prepared_tasks: &BTreeMap<TaskId, PreparedTask>,
) -> Result<(), PersistenceError> {
    let mut claims = CurrencyClaimBook::new();
    let mut payment_address_claims = BTreeMap::new();
    let mut preallocated = BTreeSet::new();

    for prepared in prepared_tasks.values() {
        prepared
            .restore_claims(&mut claims)
            .map_err(|_| PersistenceError::InvalidSnapshot)?;
        prepared
            .restore_payment_address_claims(&mut payment_address_claims)
            .map_err(|_| PersistenceError::InvalidSnapshot)?;

        for operation in &prepared.operations {
            let addresses = match operation {
                PreparedOperation::Issue { addresses, .. } => Some(addresses.as_slice()),
                PreparedOperation::LeakRepair {
                    replacement_reserve,
                    ..
                } => Some(replacement_reserve.as_slice()),
                PreparedOperation::Transfer { .. }
                | PreparedOperation::Destroy { .. }
                | PreparedOperation::RegisterPaymentAddress { .. }
                | PreparedOperation::RetirePaymentAddress { .. }
                | PreparedOperation::FinalizePaymentAddressRetirement { .. } => None,
            };

            if let Some(addresses) = addresses {
                for address in addresses {
                    if !preallocated.insert(*address) {
                        return Err(PersistenceError::InvalidSnapshot);
                    }
                }
            }
        }

        let mut candidate = state.clone();
        let mut working = candidate.business.clone();
        prepared
            .apply(&mut candidate, &mut working)
            .map_err(|_| PersistenceError::InvalidSnapshot)?;
    }

    Ok(())
}

pub(super) fn validate_prepared_snapshot_links(
    task_bindings: &BTreeMap<TaskId, TaskBinding>,
    payment_addresses: &BTreeMap<PaymentAddress, PaymentAddressRecord>,
    payment_executions: &BTreeMap<OperationClaimId, PaymentExecution>,
    prepared_tasks: &BTreeMap<TaskId, PreparedTask>,
    validator_vote_locks: &BTreeMap<(ValidatorId, ConsensusScope), [u8; 32]>,
) -> Result<(), PersistenceError> {
    let mut active_transfer_claims = BTreeSet::new();

    for (task_id, prepared) in prepared_tasks {
        let binding = task_bindings
            .get(task_id)
            .ok_or(PersistenceError::InvalidSnapshot)?;
        if binding.succeeded || binding.request_digest != prepared.request_digest {
            return Err(PersistenceError::InvalidSnapshot);
        }

        let mut expected_transfer_claims = BTreeSet::new();
        for (index, operation) in prepared.operations.iter().enumerate() {
            let operation_index =
                u64::try_from(index).map_err(|_| PersistenceError::InvalidSnapshot)?;
            let claim_id = OperationClaimId::new(task_id.clone(), operation_index);

            if let PreparedOperation::Transfer { transfer, .. } = operation {
                let execution = payment_executions
                    .get(&claim_id)
                    .ok_or(PersistenceError::InvalidSnapshot)?;
                if !execution.matches(transfer.source, transfer.destination, transfer.amount) {
                    return Err(PersistenceError::InvalidSnapshot);
                }
                expected_transfer_claims.insert(claim_id.clone());
                active_transfer_claims.insert(claim_id);
            }
        }

        if payment_executions
            .keys()
            .filter(|claim_id| claim_id.task_id() == task_id)
            .any(|claim_id| !expected_transfer_claims.contains(claim_id))
        {
            return Err(PersistenceError::InvalidSnapshot);
        }
    }

    for (claim_id, execution) in payment_executions {
        if active_transfer_claims.contains(claim_id) {
            continue;
        }

        let source = payment_addresses
            .get(&execution.source)
            .ok_or(PersistenceError::InvalidSnapshot)?;
        let destination = payment_addresses
            .get(&execution.destination)
            .ok_or(PersistenceError::InvalidSnapshot)?;
        if source.status == PaymentAddressStatus::Retired
            || destination.status == PaymentAddressStatus::Retired
        {
            return Err(PersistenceError::InvalidSnapshot);
        }
    }

    for ((_, scope), locked_digest) in validator_vote_locks {
        let ConsensusScope::PreparedTask(task_id) = scope else {
            continue;
        };
        let binding = task_bindings
            .get(task_id)
            .ok_or(PersistenceError::InvalidSnapshot)?;
        let Some(prepared) = prepared_tasks.get(task_id) else {
            if !binding.succeeded {
                return Err(PersistenceError::InvalidSnapshot);
            }
            continue;
        };

        if prepared.phase == PreparedTaskPhase::Prepared || binding.succeeded {
            return Err(PersistenceError::InvalidSnapshot);
        }

        let expected_digest = prepared
            .plan_digest()
            .map_err(|_| PersistenceError::InvalidSnapshot)?;
        if locked_digest != &expected_digest {
            return Err(PersistenceError::InvalidSnapshot);
        }
    }

    Ok(())
}
