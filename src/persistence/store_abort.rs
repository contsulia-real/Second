//! Atomic installation of certified task cancellation; never a timeout cleanup.
use super::StateStore;
use super::codec::SnapshotContents;
use super::snapshot_validation::resolve_validator_set;
use super::store_prepared::retained_sets_for_prepared;
use crate::prepared_plan::PreparedTaskPhase;
use crate::state::TaskOutcome;
use crate::{ConsensusScope, FinalityCertificate, FinalityStatement, PersistenceError, TaskId};

impl StateStore {
    pub fn prepared_abort_statement(
        &self,
        task_id: &TaskId,
    ) -> Result<FinalityStatement, PersistenceError> {
        let snapshot = self.load()?.ok_or(PersistenceError::MissingSnapshot)?;
        let prepared = abort_context(&snapshot, task_id)?;
        let validators = resolve_validator_set(
            &snapshot.validator_set,
            &snapshot.retained_validator_sets,
            prepared.validator_set_version,
        )
        .ok_or(PersistenceError::InvalidSnapshot)?;
        Ok(crate::task_abort::statement(
            task_id,
            prepared.request_digest,
            validators,
        ))
    }

    // A local plan or certified handoff binds the exact original committee.
    // Neither arbitrary remote version hints nor current-set votes authorize release.
    pub fn install_prepared_abort(
        &self,
        task_id: &TaskId,
        certificate: &FinalityCertificate,
    ) -> Result<u64, PersistenceError> {
        let _guard = self.lock()?;
        let mut latest = self
            .load_unlocked()?
            .ok_or(PersistenceError::MissingSnapshot)?;
        let binding = latest
            .state
            .protocol
            .task_bindings
            .get(task_id)
            .ok_or(PersistenceError::StalePreparedTasks)?;
        if let TaskOutcome::Cancelled(previous) = &binding.outcome {
            if *previous != certificate.statement() {
                return Err(PersistenceError::StalePreparedTasks);
            }
            let validators = resolve_validator_set(
                &latest.validator_set,
                &latest.retained_validator_sets,
                previous.validator_set_version(),
            )
            .ok_or(PersistenceError::InvalidSnapshot)?;
            certificate
                .verify(validators)
                .map_err(PersistenceError::CheckpointFinality)?;
            return Ok(latest.generation);
        }
        if binding.outcome.is_terminal() {
            return Err(PersistenceError::StalePreparedTasks);
        }
        let prepared = abort_context(&latest, task_id)?;
        if prepared.phase == PreparedTaskPhase::Finalized {
            return Err(PersistenceError::StalePreparedTasks);
        }
        let validators = resolve_validator_set(
            &latest.validator_set,
            &latest.retained_validator_sets,
            prepared.validator_set_version,
        )
        .ok_or(PersistenceError::InvalidSnapshot)?;
        let expected = crate::task_abort::statement(task_id, binding.request_digest, validators);
        if certificate.statement() != expected {
            return Err(PersistenceError::StalePreparedTasks);
        }
        certificate
            .verify(validators)
            .map_err(PersistenceError::CheckpointFinality)?;
        let scope = ConsensusScope::PreparedTask(task_id.clone());
        if latest
            .validator_vote_locks
            .iter()
            .any(|((_, locked_scope), digest)| {
                locked_scope == &scope && *digest != expected.subject_digest()
            })
        {
            return Err(PersistenceError::StalePreparedTasks);
        }
        let binding = latest
            .state
            .protocol
            .task_bindings
            .get_mut(task_id)
            .unwrap();
        binding.outcome = TaskOutcome::Cancelled(certificate.statement());
        binding.allocation_task = None;
        binding.allocation_certificate = None;
        latest
            .state
            .prerequisite
            .payment_executions
            .retain(|claim, _| claim.task_id() != task_id);
        latest.prepared_tasks.remove(task_id);
        latest
            .bft_local_states
            .retain(|(_, candidate_scope), _| candidate_scope != &scope);
        let retained_validator_sets = retained_sets_for_prepared(
            Some(&latest),
            &latest.validator_set,
            &latest.state,
            &latest.prepared_tasks,
            &latest.task_receipts,
        )?;
        self.write_next_unlocked(
            Some(latest.generation),
            SnapshotContents {
                task_receipts: &latest.task_receipts,
                pending_governance: Some(&latest.pending_governance),
                state: &latest.state,
                validator_set: &latest.validator_set,
                retained_validator_sets: &retained_validator_sets,
                public_checkpoint_proof: latest.public_checkpoint_proof.as_ref(),
                public_checkpoint_baseline: latest.public_checkpoint_baseline.as_ref(),
                latest_public_delta: latest.latest_public_delta.as_ref(),
                public_checkpoint_states: latest.public_checkpoint_states.as_ref(),
                validator_transition_proofs: &latest.validator_transition_proofs,
                recovery_checkpoint_proof: None,
                checkpoint_floor_epoch: latest.checkpoint_floor_epoch,
                recovery_checkpoint_floors: &latest.recovery_checkpoint_floors,
                validator_safety_ready: latest.validator_safety_ready,
                minimum_signing_validator_set_version: latest.minimum_signing_validator_set_version,
                pending_validator_safety_recovery: latest
                    .pending_validator_safety_recovery
                    .as_ref(),
                validator_registry: &latest.validator_registry,
                prepared_tasks: &latest.prepared_tasks,
                validator_vote_locks: &latest.validator_vote_locks,
                bft_local_states: &latest.bft_local_states,
            },
        )
    }
}

fn abort_context<'a>(
    snapshot: &'a super::PersistedNodeState,
    task_id: &TaskId,
) -> Result<&'a crate::prepared_plan::PreparedTask, PersistenceError> {
    snapshot
        .prepared_tasks
        .get(task_id)
        .or_else(|| {
            snapshot
                .state
                .protocol
                .task_handoff
                .as_ref()
                .and_then(|handoff| handoff.task_context(task_id))
        })
        .ok_or(PersistenceError::StalePreparedTasks)
}
