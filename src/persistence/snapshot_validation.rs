use std::collections::{BTreeMap, BTreeSet};

use crate::payment::{PaymentAddressRecord, PaymentExecution};
use crate::prepared_plan::{PreparedOperation, PreparedTask, PreparedTaskPhase};
use crate::state::TaskBinding;
use crate::validator_signer::FinalityScope;
use crate::{
    CurrencyClaimBook, OperationClaimId, PaymentAddress, PersistenceError, SecondState, TaskId,
    ValidatorId, ValidatorRegistry,
};

pub(super) fn validate_vote_lock_registry(
    validator_registry: &ValidatorRegistry,
    validator_vote_locks: &BTreeMap<(ValidatorId, FinalityScope), [u8; 32]>,
) -> Result<(), PersistenceError> {
    if validator_vote_locks
        .keys()
        .any(|(validator_id, _)| !validator_registry.contains(*validator_id))
    {
        return Err(PersistenceError::InvalidSnapshot);
    }

    Ok(())
}

pub(super) fn validate_prepared_plans_against_state(
    state: &SecondState,
    prepared_tasks: &BTreeMap<TaskId, PreparedTask>,
) -> Result<(), PersistenceError> {
    let mut claims = CurrencyClaimBook::new();
    let mut preallocated = BTreeSet::new();

    for prepared in prepared_tasks.values() {
        prepared
            .restore_claims(&mut claims)
            .map_err(|_| PersistenceError::InvalidSnapshot)?;

        for operation in &prepared.operations {
            let addresses = match operation {
                PreparedOperation::Issue { addresses, .. } => Some(addresses.as_slice()),
                PreparedOperation::LeakRepair {
                    replacement_reserve,
                    ..
                } => Some(replacement_reserve.as_slice()),
                PreparedOperation::Transfer { .. } | PreparedOperation::Destroy { .. } => None,
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
    validator_vote_locks: &BTreeMap<(ValidatorId, FinalityScope), [u8; 32]>,
) -> Result<(), PersistenceError> {
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
                let source = payment_addresses
                    .get(&transfer.source)
                    .ok_or(PersistenceError::InvalidSnapshot)?;
                let destination = payment_addresses
                    .get(&transfer.destination)
                    .ok_or(PersistenceError::InvalidSnapshot)?;
                if source.account != transfer.source_account
                    || destination.account != transfer.destination_account
                {
                    return Err(PersistenceError::InvalidSnapshot);
                }

                let execution = payment_executions
                    .get(&claim_id)
                    .ok_or(PersistenceError::InvalidSnapshot)?;
                if !execution.matches(transfer.source, transfer.destination, transfer.amount) {
                    return Err(PersistenceError::InvalidSnapshot);
                }
                expected_transfer_claims.insert(claim_id);
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

    for ((_, scope), locked_digest) in validator_vote_locks {
        let FinalityScope::PreparedTask(task_id) = scope else {
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
