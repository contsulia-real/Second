use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::{PersistenceError, SecondState, ValidatorRegistry, ValidatorSet};

use super::PersistedNodeState;
use super::codec::{SnapshotContents, encode_snapshot};
use super::read_cache::{ReadCache, ReadToken};
use super::slot::{
    load_latest, lock_store_file, remove_slots, shared_path_lock, slot_path, write_slots,
};

pub(super) struct StateStoreGuard<'a> {
    _process_guard: MutexGuard<'a, ()>,
    file: File,
}

impl Drop for StateStoreGuard<'_> {
    fn drop(&mut self) {
        let _ = File::unlock(&self.file);
    }
}

#[derive(Clone)]
pub struct StateStore {
    base_path: PathBuf,
    write_lock: Arc<Mutex<()>>,
    read_cache: Arc<Mutex<Option<ReadCache<PersistedNodeState>>>>,
}

impl std::fmt::Debug for StateStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StateStore")
            .field("base_path", &self.base_path)
            .finish_non_exhaustive()
    }
}

impl StateStore {
    pub fn new(path: impl AsRef<Path>) -> Self {
        let base_path = path.as_ref().to_path_buf();
        Self {
            write_lock: shared_path_lock(&base_path),
            base_path,
            read_cache: Arc::new(Mutex::new(None)),
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
        let bft_local_states = BTreeMap::new();
        let retained_validator_sets = BTreeMap::new();
        let recovery_checkpoint_floors = BTreeMap::new();
        let public_checkpoint_states = None;
        let validator_transition_proofs = BTreeMap::new();

        self.write_next_unlocked(
            None,
            SnapshotContents {
                task_receipts: &BTreeMap::new(),
                pending_governance: None,
                state,
                validator_set,
                retained_validator_sets: &retained_validator_sets,
                public_checkpoint_proof: None,
                public_checkpoint_baseline: None,
                latest_public_delta: None,
                public_checkpoint_states: public_checkpoint_states.as_ref(),
                validator_transition_proofs: &validator_transition_proofs,
                recovery_checkpoint_proof: None,
                checkpoint_floor_epoch: 0,
                recovery_checkpoint_floors: &recovery_checkpoint_floors,
                validator_safety_ready: true,
                minimum_signing_validator_set_version: validator_set.version(),
                pending_validator_safety_recovery: None,
                validator_registry: &validator_registry,
                prepared_tasks: &prepared_tasks,
                validator_vote_locks: &validator_vote_locks,
                bft_local_states: &bft_local_states,
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
        let bft_local_states = BTreeMap::new();
        let retained_validator_sets = BTreeMap::new();
        let recovery_checkpoint_floors = BTreeMap::new();
        let public_checkpoint_states = None;
        let validator_transition_proofs = BTreeMap::new();

        self.write_next_unlocked(
            None,
            SnapshotContents {
                task_receipts: &BTreeMap::new(),
                pending_governance: None,
                state,
                validator_set,
                retained_validator_sets: &retained_validator_sets,
                public_checkpoint_proof: None,
                public_checkpoint_baseline: None,
                latest_public_delta: None,
                public_checkpoint_states: public_checkpoint_states.as_ref(),
                validator_transition_proofs: &validator_transition_proofs,
                recovery_checkpoint_proof: None,
                checkpoint_floor_epoch: 0,
                recovery_checkpoint_floors: &recovery_checkpoint_floors,
                validator_safety_ready: true,
                minimum_signing_validator_set_version: validator_set.version(),
                pending_validator_safety_recovery: None,
                validator_registry,
                prepared_tasks: &prepared_tasks,
                validator_vote_locks: &validator_vote_locks,
                bft_local_states: &bft_local_states,
            },
        )
    }

    pub fn load(&self) -> Result<Option<PersistedNodeState>, PersistenceError> {
        Ok(self.load_shared()?.map(|snapshot| (*snapshot).clone()))
    }

    pub(crate) fn load_shared(&self) -> Result<Option<Arc<PersistedNodeState>>, PersistenceError> {
        let _guard = self.lock()?;
        self.load_shared_unlocked()
    }

    pub fn slot_path_for_generation(&self, generation: u64) -> PathBuf {
        slot_path(&self.base_path, generation)
    }

    pub fn remove_files(&self) -> Result<(), PersistenceError> {
        let _guard = self.lock()?;
        remove_slots(&self.base_path)
    }

    pub(super) fn lock(&self) -> Result<StateStoreGuard<'_>, PersistenceError> {
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

    pub(super) fn load_unlocked(&self) -> Result<Option<PersistedNodeState>, PersistenceError> {
        Ok(self
            .load_shared_unlocked()?
            .map(|snapshot| (*snapshot).clone()))
    }

    fn load_shared_unlocked(&self) -> Result<Option<Arc<PersistedNodeState>>, PersistenceError> {
        let token = ReadToken::read(&self.base_path)?;
        let mut cache = self
            .read_cache
            .lock()
            .map_err(|_| PersistenceError::StoreLockPoisoned)?;
        if let Some(cached) = cache.as_ref().filter(|cached| cached.token == token) {
            return Ok(Some(Arc::clone(&cached.snapshot)));
        }
        *cache = None;
        let snapshot = load_latest(&self.base_path)?.map(Arc::new);
        if let Some(snapshot) = &snapshot {
            *cache = Some(ReadCache {
                token,
                snapshot: Arc::clone(snapshot),
            });
        }
        Ok(snapshot)
    }

    pub(super) fn write_next_unlocked(
        &self,
        latest_generation: Option<u64>,
        contents: SnapshotContents<'_>,
    ) -> Result<u64, PersistenceError> {
        let generation = latest_generation
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(PersistenceError::GenerationOverflow)?;
        let refreshed = if contents.pending_governance.is_some_and(|map| {
            map.values().any(|pending| matches!(pending,
                super::PendingGovernance::CollectingTransition(transition)
                    if transition.current_validator_set_version() == contents.validator_set.version()))
        }) {
            let previous = self.load_shared_unlocked()?.ok_or(PersistenceError::MissingSnapshot)?;
            if !previous.state.same_persisted_state(contents.state)
                || &previous.prepared_tasks != contents.prepared_tasks
            {
                Some(super::governance::refresh_collecting(&contents, &previous)?)
            } else {
                None
            }
        } else {
            None
        };
        let contents = SnapshotContents {
            pending_governance: refreshed.as_ref().or(contents.pending_governance),
            ..contents
        };
        let pending = contents
            .pending_governance
            .filter(|map| map.values().any(|value| !value.retained(&contents)))
            .map(|map| {
                map.iter()
                    .filter(|(_, value)| value.retained(&contents))
                    .map(|(digest, value)| (*digest, value.clone()))
                    .collect::<BTreeMap<_, _>>()
            });
        let contents = SnapshotContents {
            task_receipts: contents.task_receipts,
            pending_governance: pending.as_ref().or(contents.pending_governance),
            ..contents
        };
        let bytes = encode_snapshot(generation, contents)?;

        write_slots(&self.base_path, generation, &bytes)?;
        // Encoding validated this exact value. Publish it directly rather than
        // decoding and validating both just-written mirrors on the next read.
        let cached = ReadToken::read(&self.base_path)
            .ok()
            .map(|token| ReadCache {
                token,
                snapshot: Arc::new(contents.owned_snapshot(generation)),
            });
        *self
            .read_cache
            .lock()
            .map_err(|_| PersistenceError::StoreLockPoisoned)? = cached;
        Ok(generation)
    }
}
