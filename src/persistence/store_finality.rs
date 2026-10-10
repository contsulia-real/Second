use crate::{ConsensusScope, PersistenceError, ValidatorId, ValidatorSet};

use super::bft_store::{open_prepared_voting, validate_bft_signing_context};
use super::codec::SnapshotContents;
use super::store::StateStore;
use super::store_recovery::{
    RecoveryCheckpointFloorUpdate, advance_recovery_checkpoint_floor_entry,
};
use super::{PersistedNodeState, VoteLockStatus};

impl StateStore {
    pub(crate) fn finality_vote_lock(
        &self,
        validator_id: ValidatorId,
        scope: ConsensusScope,
    ) -> Result<Option<[u8; 32]>, PersistenceError> {
        Ok(self.load()?.and_then(|snapshot| {
            snapshot
                .validator_vote_locks
                .get(&(validator_id, scope))
                .copied()
        }))
    }

    pub(crate) fn lock_finality_vote<F>(
        &self,
        validator_id: ValidatorId,
        scope: ConsensusScope,
        digest: [u8; 32],
        validator_set: &ValidatorSet,
        validate_latest: F,
    ) -> Result<VoteLockStatus, PersistenceError>
    where
        F: FnOnce(&PersistedNodeState) -> Result<(), PersistenceError>,
    {
        let _guard = self.lock()?;
        let mut latest = self
            .load_unlocked()?
            .ok_or(PersistenceError::MissingSnapshot)?;

        validate_bft_signing_context(&latest, validator_id, &scope, validator_set)?;

        match latest
            .validator_vote_locks
            .get(&(validator_id, scope.clone()))
        {
            Some(existing) if *existing == digest => return Ok(VoteLockStatus::AlreadyLocked),
            Some(existing) => return Ok(VoteLockStatus::Conflict(*existing)),
            None => {}
        }

        if let ConsensusScope::StateRecoveryCheckpoint {
            validator_set_version,
            serial,
        } = &scope
        {
            advance_recovery_checkpoint_floor_entry(
                &mut latest.recovery_checkpoint_floors,
                *validator_set_version,
                *serial,
                digest,
                RecoveryCheckpointFloorUpdate::Vote,
            )?;
        }

        validate_latest(&latest)?;
        self.require_bft_finality_ready(&latest, validator_id, &scope, digest)?;

        open_prepared_voting(&mut latest, &scope)?;

        latest
            .validator_vote_locks
            .insert((validator_id, scope.clone()), digest);
        // A task's immutable vote still needs its decision proof to enlist a
        // missing honest signer after restart. Terminal task writes retire it.
        if !matches!(scope, ConsensusScope::PreparedTask(_)) {
            latest.bft_local_states.remove(&(validator_id, scope));
        }

        self.write_next_unlocked(
            Some(latest.generation),
            SnapshotContents {
                task_receipts: &latest.task_receipts,
                pending_governance: Some(&latest.pending_governance),
                state: &latest.state,
                validator_set: &latest.validator_set,
                retained_validator_sets: &latest.retained_validator_sets,
                public_checkpoint_proof: latest.public_checkpoint_proof.as_ref(),
                public_checkpoint_baseline: latest.public_checkpoint_baseline.as_ref(),
                latest_public_delta: latest.latest_public_delta.as_ref(),
                public_checkpoint_states: latest.public_checkpoint_states.as_ref(),
                validator_transition_proofs: &latest.validator_transition_proofs,
                recovery_checkpoint_proof: latest.recovery_checkpoint_proof.as_ref(),
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
        )?;

        Ok(VoteLockStatus::Inserted)
    }
}
