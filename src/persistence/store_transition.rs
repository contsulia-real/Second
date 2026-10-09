use std::collections::BTreeSet;

use super::PendingValidatorSafetyRecovery;
use super::bft_store::retain_bft_states_for_validator_transition;
use super::codec::SnapshotContents;
use super::snapshot_validation::resolve_validator_set;
use super::store::StateStore;
use super::store_prepared::retained_sets_for_prepared;
use crate::{
    CertifiedValidatorSetTransition, PersistenceError, TaskId, ValidatorId, ValidatorSet,
    ValidatorSetTransitionProof, ValidatorTransitionError,
};

impl StateStore {
    pub fn activate_validator_set_transition(
        &self,
        certified_transition: &CertifiedValidatorSetTransition,
    ) -> Result<u64, PersistenceError> {
        self.activate_validator_set_transition_inner(certified_transition, None)
    }

    pub(crate) fn activate_validator_set_transition_for_runtime(
        &self,
        certified_transition: &CertifiedValidatorSetTransition,
        local_validator_id: ValidatorId,
    ) -> Result<u64, PersistenceError> {
        self.activate_validator_set_transition_inner(certified_transition, Some(local_validator_id))
    }

    fn activate_validator_set_transition_inner(
        &self,
        certified_transition: &CertifiedValidatorSetTransition,
        local_validator_id: Option<ValidatorId>,
    ) -> Result<u64, PersistenceError> {
        let _guard = self.lock()?;
        let mut latest = self
            .load_unlocked()?
            .ok_or(PersistenceError::MissingSnapshot)?;

        if latest.state.next_currency_address()
            != certified_transition.transition().currency_frontier()
        {
            return Err(PersistenceError::StaleState);
        }
        certified_transition
            .certificate()
            .verify(&latest.validator_set)
            .map_err(|error| {
                PersistenceError::ValidatorTransition(ValidatorTransitionError::Finality(error))
            })?;

        let transition = certified_transition.transition();
        let candidate = super::task_handoff::hydrate_certified(&mut latest, transition)?;
        if candidate.handoff_digest != transition.handoff_digest {
            return Err(PersistenceError::StalePreparedTasks);
        }
        latest.state.protocol.task_handoff = candidate
            .handoff
            .clone()
            .filter(|handoff| handoff.requires_commitment());
        if let Some(handoff) = &latest.state.protocol.task_handoff {
            for (task_id, task) in &handoff.requests {
                let binding = latest
                    .state
                    .protocol
                    .task_bindings
                    .get_mut(task_id)
                    .ok_or(PersistenceError::StalePreparedTasks)?;
                if !binding.outcome.is_terminal() {
                    binding.allocation_task = Some(task.clone());
                }
            }
        }

        let mut validator_registry = latest.validator_registry.clone();
        let next_validator_set = certified_transition
            .clone()
            .activate(&mut validator_registry)
            .map_err(PersistenceError::ValidatorTransition)?;
        let retained_validator_sets = retained_sets_for_prepared(
            Some(&latest),
            &next_validator_set,
            &latest.state,
            &latest.prepared_tasks,
            &latest.task_receipts,
        )?;
        let mut bft_local_states = latest.bft_local_states.clone();
        retain_bft_states_for_validator_transition(
            &mut bft_local_states,
            next_validator_set.version(),
        );

        let mut validator_transition_proofs = latest.validator_transition_proofs.clone();
        let transition_version = certified_transition
            .transition()
            .current_validator_set_version();
        if validator_transition_proofs
            .insert(
                transition_version,
                ValidatorSetTransitionProof::from_certified(certified_transition),
            )
            .is_some()
        {
            return Err(PersistenceError::InvalidSnapshot);
        }
        let pending_public_changes = BTreeSet::new();

        let pending_validator_safety_recovery = if latest.validator_safety_ready {
            None
        } else {
            local_validator_id
                .map(|validator_id| {
                    PendingValidatorSafetyRecovery::from_certified_transition(
                        validator_id,
                        &latest.validator_set,
                        certified_transition,
                    )
                })
                .transpose()?
                .flatten()
                .or_else(|| {
                    latest
                        .pending_validator_safety_recovery
                        .clone()
                        .filter(|pending| {
                            latest.validator_set.credential(pending.validator_id)
                                == next_validator_set.credential(pending.validator_id)
                                && next_validator_set.contains(pending.validator_id)
                        })
                })
        };

        self.write_next_unlocked(
            Some(latest.generation),
            SnapshotContents {
                task_receipts: &latest.task_receipts,
                pending_governance: Some(&latest.pending_governance),
                state: &latest.state,
                validator_set: &next_validator_set,
                retained_validator_sets: &retained_validator_sets,
                public_checkpoint_proof: None,
                public_checkpoint_baseline: None,
                latest_public_delta: None,
                pending_public_changes: Some(&pending_public_changes),
                validator_transition_proofs: &validator_transition_proofs,
                recovery_checkpoint_proof: None,
                checkpoint_floor_epoch: latest.checkpoint_floor_epoch,
                recovery_checkpoint_floors: &latest.recovery_checkpoint_floors,
                validator_safety_ready: latest.validator_safety_ready,
                minimum_signing_validator_set_version: latest.minimum_signing_validator_set_version,
                pending_validator_safety_recovery: pending_validator_safety_recovery.as_ref(),
                validator_registry: &validator_registry,
                prepared_tasks: &latest.prepared_tasks,
                validator_vote_locks: &latest.validator_vote_locks,
                bft_local_states: &bft_local_states,
            },
        )
    }

    pub(crate) fn validator_set_for_prepared_task(
        &self,
        task_id: &TaskId,
        expected_plan_digest: [u8; 32],
    ) -> Result<ValidatorSet, PersistenceError> {
        let snapshot = self
            .load_shared()?
            .ok_or(PersistenceError::MissingSnapshot)?;
        let prepared = snapshot
            .prepared_tasks
            .get(task_id)
            .ok_or(PersistenceError::StalePreparedTasks)?;
        let validators = resolve_validator_set(
            &snapshot.validator_set,
            &snapshot.retained_validator_sets,
            prepared.validator_set_version,
        )
        .ok_or(PersistenceError::InvalidSnapshot)?;
        if !prepared
            .candidate(expected_plan_digest)
            .map_err(|_| PersistenceError::InvalidSnapshot)?
            .is_some_and(|candidate| candidate.commit_authorized)
            && crate::task_abort::statement(task_id, prepared.request_digest, validators)
                .subject_digest()
                != expected_plan_digest
        {
            return Err(PersistenceError::StalePreparedTasks);
        }

        resolve_validator_set(
            &snapshot.validator_set,
            &snapshot.retained_validator_sets,
            prepared.validator_set_version,
        )
        .cloned()
        .ok_or(PersistenceError::InvalidSnapshot)
    }
}
