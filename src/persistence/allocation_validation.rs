use crate::prepared_plan::PreparedTask;
use crate::{CurrencyAllocation, PersistenceError, SecondState, ValidatorSet};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn referenced_versions(
    state: &SecondState,
    tasks: &BTreeMap<crate::TaskId, PreparedTask>,
) -> BTreeSet<u64> {
    tasks
        .values()
        .map(|task| task.validator_set_version)
        .chain(state.protocol.task_bindings.values().filter_map(|binding| {
            binding
                .allocation_certificate
                .as_ref()
                .map(|certificate| certificate.statement().validator_set_version())
        }))
        .chain(
            state
                .protocol
                .task_handoff
                .iter()
                .flat_map(|handoff| handoff.plans.values())
                .filter(|plan| {
                    !state
                        .protocol
                        .task_bindings
                        .get(&plan.task_id)
                        .is_some_and(|binding| binding.outcome.is_terminal())
                })
                .map(|plan| plan.validator_set_version),
        )
        .chain(state.protocol.task_bindings.values().filter_map(|binding| {
            if let crate::state::TaskOutcome::Cancelled(certificate) = &binding.outcome {
                Some(certificate.validator_set_version())
            } else {
                None
            }
        }))
        .collect()
}

pub(super) fn validate_allocations(
    state: &SecondState,
    active: &ValidatorSet,
    retained: &BTreeMap<u64, ValidatorSet>,
) -> Result<(), PersistenceError> {
    let mut ranges = BTreeMap::new();
    let mut abort_contexts = BTreeMap::new();
    for (task_id, binding) in &state.protocol.task_bindings {
        if let crate::state::TaskOutcome::Cancelled(certificate) = &binding.outcome {
            let validators = super::snapshot_validation::resolve_validator_set(
                active,
                retained,
                certificate.validator_set_version(),
            )
            .ok_or(PersistenceError::InvalidSnapshot)?;
            let context = abort_contexts
                .entry(validators.version())
                .or_insert_with(|| crate::task_abort::StatementContext::new(validators));
            if *certificate != context.statement(task_id, binding.request_digest)
                || binding.allocation_task.is_some()
                || binding.allocation_certificate.is_some()
            {
                return Err(PersistenceError::InvalidSnapshot);
            }
        }
        let Some((start, count)) = binding.allocation else {
            if binding.allocation_certificate.is_some() {
                return Err(PersistenceError::InvalidSnapshot);
            }
            continue;
        };
        let end = start
            .checked_add(count)
            .filter(|end| count > 0 && *end <= state.next_currency_address())
            .ok_or(PersistenceError::InvalidSnapshot)?;
        if ranges.insert(start, end).is_some() {
            return Err(PersistenceError::InvalidSnapshot);
        }
        if let Some(certificate) = &binding.allocation_certificate {
            let version = certificate.statement().validator_set_version();
            let validators =
                super::snapshot_validation::resolve_validator_set(active, retained, version)
                    .ok_or(PersistenceError::InvalidSnapshot)?;
            certificate
                .verify(validators)
                .map_err(|_| PersistenceError::InvalidSnapshot)?;
            if certificate.statement().subject_digest()
                != CurrencyAllocation::digest_for(version, start, count, binding.request_digest)
            {
                return Err(PersistenceError::InvalidSnapshot);
            }
        }
    }
    let mut previous_end = None;
    for (start, end) in ranges {
        if previous_end.is_some_and(|previous| start < previous) {
            return Err(PersistenceError::InvalidSnapshot);
        }
        previous_end = Some(end);
    }
    Ok(())
}
