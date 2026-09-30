use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::prepared_plan::PreparedTask;
use crate::validator_signer::FinalityScope;
use crate::{
    CertifiedPublicCurrencyCheckpoint, PersistenceError, PublicCurrencyCheckpointProof,
    SecondState, TaskId, ValidatorId, ValidatorRegistry, ValidatorSet,
};

use super::codec::{SnapshotContents, encode_snapshot};
use super::slot::{load_latest, remove_slots, shared_path_lock, slot_path, write_slot};
use super::{PersistedNodeState, VoteLockStatus};

#[derive(Clone, Debug)]
pub struct StateStore {
    base_path: PathBuf,
    write_lock: Arc<Mutex<()>>,
}

impl StateStore {
    pub fn new(path: impl AsRef<Path>) -> Self {
        let base_path = path.as_ref().to_path_buf();
        Self {
            write_lock: shared_path_lock(&base_path),
            base_path,
        }
    }

    pub fn save(
        &self,
        state: &SecondState,
        validator_set: &ValidatorSet,
    ) -> Result<u64, PersistenceError> {
        let _guard = self.lock()?;
        let latest = self.load_unlocked()?;
        let registry = registry_for_write(latest.as_ref(), validator_set)?;
        let checkpoint_floor_epoch = checkpoint_floor_for_write(latest.as_ref(), None)?;
        let checkpoint = latest
            .as_ref()
            .and_then(|snapshot| snapshot.public_checkpoint_proof.clone())
            .filter(|proof| checkpoint_matches(proof, state, validator_set));
        let prepared_tasks = latest
            .as_ref()
            .map(|snapshot| snapshot.prepared_tasks.clone())
            .unwrap_or_default();
        let vote_locks = latest
            .as_ref()
            .map(|snapshot| snapshot.validator_vote_locks.clone())
            .unwrap_or_default();

        self.write_next_unlocked(
            latest.as_ref().map(|snapshot| snapshot.generation),
            SnapshotContents {
                state,
                validator_set,
                public_checkpoint_proof: checkpoint.as_ref(),
                checkpoint_floor_epoch,
                validator_registry: &registry,
                prepared_tasks: &prepared_tasks,
                validator_vote_locks: &vote_locks,
            },
        )
    }

    pub fn save_with_checkpoint_proof(
        &self,
        state: &SecondState,
        validator_set: &ValidatorSet,
        public_checkpoint_proof: Option<&PublicCurrencyCheckpointProof>,
    ) -> Result<u64, PersistenceError> {
        let _guard = self.lock()?;
        let latest = self.load_unlocked()?;
        let registry = registry_for_write(latest.as_ref(), validator_set)?;
        let checkpoint_floor_epoch = checkpoint_floor_for_write(latest.as_ref(), None)?;
        let prepared_tasks = latest
            .as_ref()
            .map(|snapshot| snapshot.prepared_tasks.clone())
            .unwrap_or_default();
        let vote_locks = latest
            .as_ref()
            .map(|snapshot| snapshot.validator_vote_locks.clone())
            .unwrap_or_default();

        self.write_next_unlocked(
            latest.as_ref().map(|snapshot| snapshot.generation),
            SnapshotContents {
                state,
                validator_set,
                public_checkpoint_proof,
                checkpoint_floor_epoch,
                validator_registry: &registry,
                prepared_tasks: &prepared_tasks,
                validator_vote_locks: &vote_locks,
            },
        )
    }

    pub fn save_with_certified_checkpoint(
        &self,
        state: &SecondState,
        validator_set: &ValidatorSet,
        certified_checkpoint: &CertifiedPublicCurrencyCheckpoint,
    ) -> Result<u64, PersistenceError> {
        let proof = certified_checkpoint.to_unverified_proof();
        validate_certified_checkpoint_for_state(state, validator_set, certified_checkpoint)?;

        let _guard = self.lock()?;
        let latest = self.load_unlocked()?;
        let registry = registry_for_write(latest.as_ref(), validator_set)?;
        let checkpoint_floor_epoch = checkpoint_floor_for_write(latest.as_ref(), Some(&proof))?;
        let prepared_tasks = latest
            .as_ref()
            .map(|snapshot| snapshot.prepared_tasks.clone())
            .unwrap_or_default();
        let vote_locks = latest
            .as_ref()
            .map(|snapshot| snapshot.validator_vote_locks.clone())
            .unwrap_or_default();

        self.write_next_unlocked(
            latest.as_ref().map(|snapshot| snapshot.generation),
            SnapshotContents {
                state,
                validator_set,
                public_checkpoint_proof: Some(&proof),
                checkpoint_floor_epoch,
                validator_registry: &registry,
                prepared_tasks: &prepared_tasks,
                validator_vote_locks: &vote_locks,
            },
        )
    }

    pub fn save_with_validator_registry(
        &self,
        state: &SecondState,
        validator_set: &ValidatorSet,
        validator_registry: &ValidatorRegistry,
    ) -> Result<u64, PersistenceError> {
        validator_registry
            .validate_current_set(validator_set)
            .map_err(|_| PersistenceError::ValidatorRegistryMismatch)?;

        let _guard = self.lock()?;
        let latest = self.load_unlocked()?;
        let checkpoint_floor_epoch = checkpoint_floor_for_write(latest.as_ref(), None)?;
        let checkpoint = latest
            .as_ref()
            .and_then(|snapshot| snapshot.public_checkpoint_proof.clone())
            .filter(|proof| checkpoint_matches(proof, state, validator_set));
        let prepared_tasks = latest
            .as_ref()
            .map(|snapshot| snapshot.prepared_tasks.clone())
            .unwrap_or_default();
        let vote_locks = latest
            .as_ref()
            .map(|snapshot| snapshot.validator_vote_locks.clone())
            .unwrap_or_default();

        self.write_next_unlocked(
            latest.as_ref().map(|snapshot| snapshot.generation),
            SnapshotContents {
                state,
                validator_set,
                public_checkpoint_proof: checkpoint.as_ref(),
                checkpoint_floor_epoch,
                validator_registry,
                prepared_tasks: &prepared_tasks,
                validator_vote_locks: &vote_locks,
            },
        )
    }

    pub(crate) fn save_with_prepared(
        &self,
        state: &SecondState,
        validator_set: &ValidatorSet,
        prepared_tasks: &BTreeMap<TaskId, PreparedTask>,
    ) -> Result<u64, PersistenceError> {
        let _guard = self.lock()?;
        let latest = self.load_unlocked()?;
        let registry = registry_for_write(latest.as_ref(), validator_set)?;
        let checkpoint_floor_epoch = checkpoint_floor_for_write(latest.as_ref(), None)?;
        let checkpoint = latest
            .as_ref()
            .and_then(|snapshot| snapshot.public_checkpoint_proof.clone())
            .filter(|proof| checkpoint_matches(proof, state, validator_set));
        let vote_locks = latest
            .as_ref()
            .map(|snapshot| snapshot.validator_vote_locks.clone())
            .unwrap_or_default();

        self.write_next_unlocked(
            latest.as_ref().map(|snapshot| snapshot.generation),
            SnapshotContents {
                state,
                validator_set,
                public_checkpoint_proof: checkpoint.as_ref(),
                checkpoint_floor_epoch,
                validator_registry: &registry,
                prepared_tasks,
                validator_vote_locks: &vote_locks,
            },
        )
    }

    pub(crate) fn replace_prepared_tasks(
        &self,
        prepared_tasks: &BTreeMap<TaskId, PreparedTask>,
    ) -> Result<u64, PersistenceError> {
        let _guard = self.lock()?;
        let latest = self
            .load_unlocked()?
            .ok_or(PersistenceError::MissingSnapshot)?;

        self.write_next_unlocked(
            Some(latest.generation),
            SnapshotContents {
                state: &latest.state,
                validator_set: &latest.validator_set,
                public_checkpoint_proof: latest.public_checkpoint_proof.as_ref(),
                checkpoint_floor_epoch: latest.checkpoint_floor_epoch,
                validator_registry: &latest.validator_registry,
                prepared_tasks,
                validator_vote_locks: &latest.validator_vote_locks,
            },
        )
    }

    pub(crate) fn load_prepared_tasks(
        &self,
    ) -> Result<BTreeMap<TaskId, PreparedTask>, PersistenceError> {
        Ok(self
            .load()?
            .map(|snapshot| snapshot.prepared_tasks)
            .unwrap_or_default())
    }

    pub(crate) fn finality_vote_lock(
        &self,
        validator_id: ValidatorId,
        scope: FinalityScope,
    ) -> Result<Option<[u8; 32]>, PersistenceError> {
        Ok(self.load()?.and_then(|snapshot| {
            snapshot
                .validator_vote_locks
                .get(&(validator_id, scope))
                .copied()
        }))
    }

    pub(crate) fn lock_finality_vote(
        &self,
        validator_id: ValidatorId,
        scope: FinalityScope,
        digest: [u8; 32],
    ) -> Result<VoteLockStatus, PersistenceError> {
        let _guard = self.lock()?;
        let mut latest = self
            .load_unlocked()?
            .ok_or(PersistenceError::MissingSnapshot)?;

        match latest
            .validator_vote_locks
            .get(&(validator_id, scope.clone()))
        {
            Some(existing) if *existing == digest => return Ok(VoteLockStatus::AlreadyLocked),
            Some(existing) => return Ok(VoteLockStatus::Conflict(*existing)),
            None => {}
        }

        latest
            .validator_vote_locks
            .insert((validator_id, scope), digest);

        self.write_next_unlocked(
            Some(latest.generation),
            SnapshotContents {
                state: &latest.state,
                validator_set: &latest.validator_set,
                public_checkpoint_proof: latest.public_checkpoint_proof.as_ref(),
                checkpoint_floor_epoch: latest.checkpoint_floor_epoch,
                validator_registry: &latest.validator_registry,
                prepared_tasks: &latest.prepared_tasks,
                validator_vote_locks: &latest.validator_vote_locks,
            },
        )?;

        Ok(VoteLockStatus::Inserted)
    }

    pub fn load(&self) -> Result<Option<PersistedNodeState>, PersistenceError> {
        let _guard = self.lock()?;
        self.load_unlocked()
    }

    pub fn slot_path_for_generation(&self, generation: u64) -> PathBuf {
        slot_path(&self.base_path, generation)
    }

    pub fn remove_files(&self) -> Result<(), PersistenceError> {
        let _guard = self.lock()?;
        remove_slots(&self.base_path)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, ()>, PersistenceError> {
        self.write_lock
            .lock()
            .map_err(|_| PersistenceError::StoreLockPoisoned)
    }

    fn load_unlocked(&self) -> Result<Option<PersistedNodeState>, PersistenceError> {
        load_latest(&self.base_path)
    }

    fn write_next_unlocked(
        &self,
        latest_generation: Option<u64>,
        contents: SnapshotContents<'_>,
    ) -> Result<u64, PersistenceError> {
        let generation = latest_generation
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(PersistenceError::GenerationOverflow)?;
        let bytes = encode_snapshot(generation, contents)?;

        write_slot(&self.base_path, generation, &bytes)?;
        Ok(generation)
    }
}

fn registry_for_write(
    latest: Option<&PersistedNodeState>,
    validator_set: &ValidatorSet,
) -> Result<ValidatorRegistry, PersistenceError> {
    match latest {
        Some(snapshot) => {
            snapshot
                .validator_registry
                .validate_current_set(validator_set)
                .map_err(|_| PersistenceError::ValidatorRegistryMismatch)?;
            Ok(snapshot.validator_registry.clone())
        }
        None => ValidatorRegistry::from_validator_set(validator_set)
            .map_err(|_| PersistenceError::ValidatorRegistryMismatch),
    }
}

fn validate_certified_checkpoint_for_state(
    state: &SecondState,
    validator_set: &ValidatorSet,
    checkpoint: &CertifiedPublicCurrencyCheckpoint,
) -> Result<(), PersistenceError> {
    if checkpoint.checkpoint().summary() != &state.public_currency_summary() {
        return Err(PersistenceError::CheckpointDoesNotMatchState);
    }

    let actual = checkpoint.certificate().statement().validator_set_version();
    if actual != validator_set.version() {
        return Err(PersistenceError::CheckpointValidatorSetMismatch {
            expected: validator_set.version(),
            actual,
        });
    }

    Ok(())
}

fn checkpoint_floor_for_write(
    latest: Option<&PersistedNodeState>,
    new_checkpoint: Option<&PublicCurrencyCheckpointProof>,
) -> Result<u64, PersistenceError> {
    let minimum = latest
        .map(|snapshot| snapshot.checkpoint_floor_epoch)
        .unwrap_or(0);
    let Some(proof) = new_checkpoint else {
        return Ok(minimum);
    };

    let actual = proof.checkpoint().epoch();
    if actual < minimum {
        return Err(PersistenceError::StaleCheckpointEpoch { minimum, actual });
    }

    Ok(actual)
}

fn checkpoint_matches(
    proof: &PublicCurrencyCheckpointProof,
    state: &SecondState,
    validator_set: &ValidatorSet,
) -> bool {
    proof.checkpoint().summary() == &state.public_currency_summary()
        && proof.validator_set_version() == validator_set.version()
}
