use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::prepared_plan::PreparedTask;
use crate::validator_signer::FinalityScope;
use crate::{
    CertifiedPublicCurrencyCheckpoint, CertifiedValidatorSetTransition, PersistenceError,
    PublicCurrencyCheckpointProof, SecondState, TaskId, ValidatorId, ValidatorRegistry,
    ValidatorSet, ValidatorTransitionError,
};

use super::codec::{SnapshotContents, encode_snapshot};
use super::slot::{
    load_latest, lock_store_file, remove_slots, shared_path_lock, slot_path, write_slots,
};
use super::snapshot_validation::resolve_validator_set;
use super::{PersistedNodeState, VoteLockStatus};

struct StateStoreGuard<'a> {
    _process_guard: MutexGuard<'a, ()>,
    file: File,
}

impl Drop for StateStoreGuard<'_> {
    fn drop(&mut self) {
        let _ = File::unlock(&self.file);
    }
}

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

    pub(crate) fn base_path(&self) -> &Path {
        &self.base_path
    }

    pub fn initialize(
        &self,
        state: &SecondState,
        validator_set: &ValidatorSet,
    ) -> Result<u64, PersistenceError> {
        let _guard = self.lock()?;
        if self.load_unlocked()?.is_some() {
            return Err(PersistenceError::AlreadyInitialized);
        }

        let validator_registry = ValidatorRegistry::from_validator_set(validator_set)
            .map_err(|_| PersistenceError::ValidatorRegistryMismatch)?;
        let prepared_tasks = BTreeMap::new();
        let validator_vote_locks = BTreeMap::new();
        let retained_validator_sets = BTreeMap::new();

        self.write_next_unlocked(
            None,
            SnapshotContents {
                state,
                validator_set,
                retained_validator_sets: &retained_validator_sets,
                public_checkpoint_proof: None,
                checkpoint_floor_epoch: 0,
                validator_registry: &validator_registry,
                prepared_tasks: &prepared_tasks,
                validator_vote_locks: &validator_vote_locks,
            },
        )
    }

    pub fn initialize_with_validator_registry(
        &self,
        state: &SecondState,
        validator_set: &ValidatorSet,
        validator_registry: &ValidatorRegistry,
    ) -> Result<u64, PersistenceError> {
        let _guard = self.lock()?;
        if self.load_unlocked()?.is_some() {
            return Err(PersistenceError::AlreadyInitialized);
        }

        validator_registry
            .validate_current_set(validator_set)
            .map_err(|_| PersistenceError::ValidatorRegistryMismatch)?;
        let prepared_tasks = BTreeMap::new();
        let validator_vote_locks = BTreeMap::new();
        let retained_validator_sets = BTreeMap::new();

        self.write_next_unlocked(
            None,
            SnapshotContents {
                state,
                validator_set,
                retained_validator_sets: &retained_validator_sets,
                public_checkpoint_proof: None,
                checkpoint_floor_epoch: 0,
                validator_registry,
                prepared_tasks: &prepared_tasks,
                validator_vote_locks: &validator_vote_locks,
            },
        )
    }

    pub fn activate_validator_set_transition(
        &self,
        certified_transition: &CertifiedValidatorSetTransition,
    ) -> Result<u64, PersistenceError> {
        let _guard = self.lock()?;
        let latest = self
            .load_unlocked()?
            .ok_or(PersistenceError::MissingSnapshot)?;

        certified_transition
            .certificate()
            .verify(&latest.validator_set)
            .map_err(|error| {
                PersistenceError::ValidatorTransition(ValidatorTransitionError::Finality(error))
            })?;

        let mut validator_registry = latest.validator_registry.clone();
        let next_validator_set = certified_transition
            .clone()
            .activate(&mut validator_registry)
            .map_err(PersistenceError::ValidatorTransition)?;
        let retained_validator_sets =
            retained_sets_for_prepared(Some(&latest), &next_validator_set, &latest.prepared_tasks)?;

        self.write_next_unlocked(
            Some(latest.generation),
            SnapshotContents {
                state: &latest.state,
                validator_set: &next_validator_set,
                retained_validator_sets: &retained_validator_sets,
                public_checkpoint_proof: None,
                checkpoint_floor_epoch: latest.checkpoint_floor_epoch,
                validator_registry: &validator_registry,
                prepared_tasks: &latest.prepared_tasks,
                validator_vote_locks: &latest.validator_vote_locks,
            },
        )
    }

    pub(crate) fn validator_set_for_prepared_task(
        &self,
        task_id: &TaskId,
        expected_plan_digest: [u8; 32],
    ) -> Result<ValidatorSet, PersistenceError> {
        let snapshot = self.load()?.ok_or(PersistenceError::MissingSnapshot)?;
        let prepared = snapshot
            .prepared_tasks
            .get(task_id)
            .ok_or(PersistenceError::StalePreparedTasks)?;
        let durable_plan_digest = prepared
            .plan_digest()
            .map_err(|_| PersistenceError::InvalidSnapshot)?;
        if durable_plan_digest != expected_plan_digest {
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

    pub fn attach_checkpoint_proof(
        &self,
        public_checkpoint_proof: Option<&PublicCurrencyCheckpointProof>,
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
                retained_validator_sets: &latest.retained_validator_sets,
                public_checkpoint_proof,
                checkpoint_floor_epoch: latest.checkpoint_floor_epoch,
                validator_registry: &latest.validator_registry,
                prepared_tasks: &latest.prepared_tasks,
                validator_vote_locks: &latest.validator_vote_locks,
            },
        )
    }

    pub fn attach_certified_checkpoint(
        &self,
        certified_checkpoint: &CertifiedPublicCurrencyCheckpoint,
    ) -> Result<u64, PersistenceError> {
        let _guard = self.lock()?;
        let latest = self
            .load_unlocked()?
            .ok_or(PersistenceError::MissingSnapshot)?;

        validate_certified_checkpoint_for_state(
            &latest.state,
            &latest.validator_set,
            certified_checkpoint,
        )?;
        let proof = certified_checkpoint.to_unverified_proof();
        let checkpoint_floor_epoch = checkpoint_floor_for_write(Some(&latest), Some(&proof))?;

        self.write_next_unlocked(
            Some(latest.generation),
            SnapshotContents {
                state: &latest.state,
                validator_set: &latest.validator_set,
                retained_validator_sets: &latest.retained_validator_sets,
                public_checkpoint_proof: Some(&proof),
                checkpoint_floor_epoch,
                validator_registry: &latest.validator_registry,
                prepared_tasks: &latest.prepared_tasks,
                validator_vote_locks: &latest.validator_vote_locks,
            },
        )
    }

    pub fn advance_checkpoint_floor(
        &self,
        checkpoint: &CertifiedPublicCurrencyCheckpoint,
    ) -> Result<u64, PersistenceError> {
        let _guard = self.lock()?;
        let latest = self
            .load_unlocked()?
            .ok_or(PersistenceError::MissingSnapshot)?;

        checkpoint
            .certificate()
            .verify(&latest.validator_set)
            .map_err(PersistenceError::CheckpointFinality)?;

        let proof = checkpoint.to_unverified_proof();
        let checkpoint_floor_epoch = checkpoint_floor_for_write(Some(&latest), Some(&proof))?;

        self.write_next_unlocked(
            Some(latest.generation),
            SnapshotContents {
                state: &latest.state,
                validator_set: &latest.validator_set,
                retained_validator_sets: &latest.retained_validator_sets,
                public_checkpoint_proof: latest.public_checkpoint_proof.as_ref(),
                checkpoint_floor_epoch,
                validator_registry: &latest.validator_registry,
                prepared_tasks: &latest.prepared_tasks,
                validator_vote_locks: &latest.validator_vote_locks,
            },
        )
    }

    pub(crate) fn save_with_prepared(
        &self,
        expected_state: &SecondState,
        state: &SecondState,
        validator_set: &ValidatorSet,
        expected_prepared_tasks: &BTreeMap<TaskId, PreparedTask>,
        prepared_tasks: &BTreeMap<TaskId, PreparedTask>,
    ) -> Result<u64, PersistenceError> {
        let _guard = self.lock()?;
        let latest = self.load_unlocked()?;

        if let Some(snapshot) = latest.as_ref() {
            if !snapshot.state.same_persisted_state(expected_state) {
                return Err(PersistenceError::StaleState);
            }
            if &snapshot.prepared_tasks != expected_prepared_tasks {
                return Err(PersistenceError::StalePreparedTasks);
            }
        }

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
        let retained_validator_sets =
            retained_sets_for_prepared(latest.as_ref(), validator_set, prepared_tasks)?;

        self.write_next_unlocked(
            latest.as_ref().map(|snapshot| snapshot.generation),
            SnapshotContents {
                state,
                validator_set,
                retained_validator_sets: &retained_validator_sets,
                public_checkpoint_proof: checkpoint.as_ref(),
                checkpoint_floor_epoch,
                validator_registry: &registry,
                prepared_tasks,
                validator_vote_locks: &vote_locks,
            },
        )
    }

    pub(crate) fn commit_prepared_state(
        &self,
        expected_state: &SecondState,
        state: &SecondState,
        expected_prepared_tasks: &BTreeMap<TaskId, PreparedTask>,
        prepared_tasks: &BTreeMap<TaskId, PreparedTask>,
    ) -> Result<u64, PersistenceError> {
        let _guard = self.lock()?;
        let latest = self
            .load_unlocked()?
            .ok_or(PersistenceError::MissingSnapshot)?;

        if !latest.state.same_persisted_state(expected_state) {
            return Err(PersistenceError::StaleState);
        }
        if &latest.prepared_tasks != expected_prepared_tasks {
            return Err(PersistenceError::StalePreparedTasks);
        }

        let retained_validator_sets =
            retained_sets_for_prepared(Some(&latest), &latest.validator_set, prepared_tasks)?;
        let checkpoint = latest
            .public_checkpoint_proof
            .as_ref()
            .filter(|proof| checkpoint_matches(proof, state, &latest.validator_set));

        self.write_next_unlocked(
            Some(latest.generation),
            SnapshotContents {
                state,
                validator_set: &latest.validator_set,
                retained_validator_sets: &retained_validator_sets,
                public_checkpoint_proof: checkpoint,
                checkpoint_floor_epoch: latest.checkpoint_floor_epoch,
                validator_registry: &latest.validator_registry,
                prepared_tasks,
                validator_vote_locks: &latest.validator_vote_locks,
            },
        )
    }

    pub(crate) fn replace_prepared_tasks(
        &self,
        expected_prepared_tasks: &BTreeMap<TaskId, PreparedTask>,
        prepared_tasks: &BTreeMap<TaskId, PreparedTask>,
    ) -> Result<u64, PersistenceError> {
        let _guard = self.lock()?;
        let latest = self
            .load_unlocked()?
            .ok_or(PersistenceError::MissingSnapshot)?;

        if &latest.prepared_tasks != expected_prepared_tasks {
            return Err(PersistenceError::StalePreparedTasks);
        }
        let retained_validator_sets =
            retained_sets_for_prepared(Some(&latest), &latest.validator_set, prepared_tasks)?;

        self.write_next_unlocked(
            Some(latest.generation),
            SnapshotContents {
                state: &latest.state,
                validator_set: &latest.validator_set,
                retained_validator_sets: &retained_validator_sets,
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

    pub(crate) fn lock_finality_vote<F>(
        &self,
        validator_id: ValidatorId,
        scope: FinalityScope,
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

        validate_vote_validator_set(&latest, &scope, validator_set)?;

        match latest
            .validator_vote_locks
            .get(&(validator_id, scope.clone()))
        {
            Some(existing) if *existing == digest => return Ok(VoteLockStatus::AlreadyLocked),
            Some(existing) => return Ok(VoteLockStatus::Conflict(*existing)),
            None => {}
        }

        validate_latest(&latest)?;

        latest
            .validator_vote_locks
            .insert((validator_id, scope), digest);

        self.write_next_unlocked(
            Some(latest.generation),
            SnapshotContents {
                state: &latest.state,
                validator_set: &latest.validator_set,
                retained_validator_sets: &latest.retained_validator_sets,
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

    fn lock(&self) -> Result<StateStoreGuard<'_>, PersistenceError> {
        let process_guard = self
            .write_lock
            .lock()
            .map_err(|_| PersistenceError::StoreLockPoisoned)?;
        let file = lock_store_file(&self.base_path)?;
        Ok(StateStoreGuard {
            _process_guard: process_guard,
            file,
        })
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

        write_slots(&self.base_path, generation, &bytes)?;
        Ok(generation)
    }
}

fn retained_sets_for_prepared(
    latest: Option<&PersistedNodeState>,
    active_validator_set: &ValidatorSet,
    prepared_tasks: &BTreeMap<TaskId, PreparedTask>,
) -> Result<BTreeMap<u64, ValidatorSet>, PersistenceError> {
    let referenced = prepared_tasks
        .values()
        .map(|prepared| prepared.validator_set_version)
        .collect::<std::collections::BTreeSet<_>>();
    let mut retained = BTreeMap::new();

    for version in referenced {
        if version == active_validator_set.version() {
            continue;
        }

        let set = latest
            .and_then(|snapshot| {
                if snapshot.validator_set.version() == version {
                    Some(snapshot.validator_set.clone())
                } else {
                    snapshot.retained_validator_sets.get(&version).cloned()
                }
            })
            .ok_or(PersistenceError::InvalidSnapshot)?;
        retained.insert(version, set);
    }

    Ok(retained)
}

fn validate_vote_validator_set(
    snapshot: &PersistedNodeState,
    scope: &FinalityScope,
    validator_set: &ValidatorSet,
) -> Result<(), PersistenceError> {
    match scope {
        FinalityScope::PreparedTask(task_id) => {
            let prepared = snapshot
                .prepared_tasks
                .get(task_id)
                .ok_or(PersistenceError::StalePreparedTasks)?;
            let expected = resolve_validator_set(
                &snapshot.validator_set,
                &snapshot.retained_validator_sets,
                prepared.validator_set_version,
            )
            .ok_or(PersistenceError::InvalidSnapshot)?;
            if expected != validator_set {
                return Err(PersistenceError::ValidatorRegistryMismatch);
            }
        }
        FinalityScope::PublicCheckpoint { .. } | FinalityScope::ValidatorSetTransition { .. } => {
            snapshot
                .validator_registry
                .validate_current_set(validator_set)
                .map_err(|_| PersistenceError::ValidatorRegistryMismatch)?;
        }
    }
    Ok(())
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
