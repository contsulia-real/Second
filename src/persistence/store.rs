use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::ConsensusScope;
use crate::prepared_plan::{PreparedTask, PreparedTaskPhase};
use crate::{
    CertifiedPublicCurrencyCheckpoint, CertifiedStateRecoveryCheckpoint,
    CertifiedValidatorSetTransition, PersistenceError, PublicCurrencyCheckpointProof, SecondState,
    StateRecoveryCheckpoint, StateRecoveryPayload, TaskId, ValidatorId, ValidatorRegistry,
    ValidatorSet, ValidatorTransitionError,
};

use super::bft_store::{
    retain_active_prepared_bft_states, retain_bft_states_for_validator_transition,
};
use super::codec::{SnapshotContents, encode_snapshot};
use super::slot::{
    load_latest, lock_store_file, remove_slots, shared_path_lock, slot_path, write_slots,
};
use super::snapshot_validation::resolve_validator_set;
use super::{PersistedNodeState, RecoveryCheckpointFloor, VoteLockStatus};

pub(super) struct StateStoreGuard<'a> {
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
        let bft_local_states = BTreeMap::new();
        let retained_validator_sets = BTreeMap::new();
        let recovery_checkpoint_floors = BTreeMap::new();

        self.write_next_unlocked(
            None,
            SnapshotContents {
                state,
                validator_set,
                retained_validator_sets: &retained_validator_sets,
                public_checkpoint_proof: None,
                checkpoint_floor_epoch: 0,
                recovery_checkpoint_floors: &recovery_checkpoint_floors,
                validator_safety_ready: true,
                minimum_signing_validator_set_version: validator_set.version(),
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

        self.write_next_unlocked(
            None,
            SnapshotContents {
                state,
                validator_set,
                retained_validator_sets: &retained_validator_sets,
                public_checkpoint_proof: None,
                checkpoint_floor_epoch: 0,
                recovery_checkpoint_floors: &recovery_checkpoint_floors,
                validator_safety_ready: true,
                minimum_signing_validator_set_version: validator_set.version(),
                validator_registry,
                prepared_tasks: &prepared_tasks,
                validator_vote_locks: &validator_vote_locks,
                bft_local_states: &bft_local_states,
            },
        )
    }

    pub fn install_recovered_state(
        &self,
        payload: &StateRecoveryPayload,
        checkpoint: &CertifiedStateRecoveryCheckpoint,
        trusted_validator_set: &ValidatorSet,
    ) -> Result<u64, PersistenceError> {
        let _guard = self.lock()?;
        if self.load_unlocked()?.is_some() {
            return Err(PersistenceError::AlreadyInitialized);
        }

        checkpoint.verify_payload(payload, trusted_validator_set)?;

        let retained_validator_sets = BTreeMap::new();
        let recovery_checkpoint_floors = BTreeMap::from([(
            checkpoint.checkpoint().validator_set_version(),
            RecoveryCheckpointFloor {
                serial: checkpoint.checkpoint().serial(),
                checkpoint_digest: checkpoint.checkpoint().digest(),
                certified: true,
            },
        )]);
        let prepared_tasks = BTreeMap::new();
        let validator_vote_locks = BTreeMap::new();
        let bft_local_states = BTreeMap::new();
        self.write_next_unlocked(
            None,
            SnapshotContents {
                state: payload.state(),
                validator_set: payload.validator_set(),
                retained_validator_sets: &retained_validator_sets,
                public_checkpoint_proof: None,
                checkpoint_floor_epoch: 0,
                recovery_checkpoint_floors: &recovery_checkpoint_floors,
                validator_safety_ready: false,
                minimum_signing_validator_set_version: payload.validator_set().version(),
                validator_registry: payload.validator_registry(),
                prepared_tasks: &prepared_tasks,
                validator_vote_locks: &validator_vote_locks,
                bft_local_states: &bft_local_states,
            },
        )
    }

    pub(crate) fn complete_validator_safety_recovery(
        &self,
        validator_id: ValidatorId,
        new_consensus_public_key: [u8; 32],
        previous_validator_set: &ValidatorSet,
        certified_transition: &CertifiedValidatorSetTransition,
    ) -> Result<u64, PersistenceError> {
        let _guard = self.lock()?;
        let latest = self
            .load_unlocked()?
            .ok_or(PersistenceError::MissingSnapshot)?;

        if latest.validator_safety_ready {
            return Err(PersistenceError::InvalidValidatorSafetyRecovery);
        }

        certified_transition
            .certificate()
            .verify(previous_validator_set)
            .map_err(|error| {
                PersistenceError::ValidatorTransition(ValidatorTransitionError::Finality(error))
            })?;

        let transition = certified_transition.transition();
        if transition.current_validator_set_version() != previous_validator_set.version()
            || certified_transition.next_validator_set() != &latest.validator_set
            || latest.validator_set.version()
                != previous_validator_set
                    .version()
                    .checked_add(1)
                    .ok_or(PersistenceError::InvalidValidatorSafetyRecovery)?
        {
            return Err(PersistenceError::InvalidValidatorSafetyRecovery);
        }

        if certified_transition
            .certificate()
            .votes()
            .iter()
            .any(|vote| vote.validator_id() == validator_id)
        {
            return Err(
                PersistenceError::RecoveringValidatorVotedSafetyFenceTransition(validator_id),
            );
        }

        latest
            .validator_registry
            .validate_current_set(&latest.validator_set)
            .map_err(|_| PersistenceError::ValidatorRegistryMismatch)?;
        latest
            .validator_registry
            .validate_historical_set(previous_validator_set)
            .map_err(|_| PersistenceError::ValidatorRegistryMismatch)?;

        if !latest
            .recovery_checkpoint_floors
            .contains_key(&latest.validator_set.version())
        {
            return Err(PersistenceError::InvalidValidatorSafetyRecovery);
        }

        let previous = previous_validator_set
            .validator(validator_id)
            .ok_or(PersistenceError::InvalidValidatorSafetyRecovery)?;
        let current = latest
            .validator_set
            .validator(validator_id)
            .ok_or(PersistenceError::InvalidValidatorSafetyRecovery)?;

        if previous.identity_public_key() != current.identity_public_key()
            || previous.recovery_public_key() != current.recovery_public_key()
            || previous.consensus_public_key() == current.consensus_public_key()
            || current.consensus_public_key() != new_consensus_public_key
        {
            return Err(PersistenceError::InvalidValidatorSafetyRecovery);
        }

        let rotation = transition
            .consensus_key_rotations()
            .iter()
            .find(|rotation| rotation.validator_id() == validator_id)
            .ok_or(PersistenceError::InvalidValidatorSafetyRecovery)?;
        rotation.verify(previous).map_err(|error| {
            PersistenceError::ValidatorTransition(ValidatorTransitionError::Rotation(error))
        })?;
        if rotation.new_consensus_public_key() != new_consensus_public_key {
            return Err(PersistenceError::InvalidValidatorSafetyRecovery);
        }

        self.write_next_unlocked(
            Some(latest.generation),
            SnapshotContents {
                state: &latest.state,
                validator_set: &latest.validator_set,
                retained_validator_sets: &latest.retained_validator_sets,
                public_checkpoint_proof: latest.public_checkpoint_proof.as_ref(),
                checkpoint_floor_epoch: latest.checkpoint_floor_epoch,
                recovery_checkpoint_floors: &latest.recovery_checkpoint_floors,
                validator_safety_ready: true,
                minimum_signing_validator_set_version: latest.validator_set.version(),
                validator_registry: &latest.validator_registry,
                prepared_tasks: &latest.prepared_tasks,
                validator_vote_locks: &latest.validator_vote_locks,
                bft_local_states: &latest.bft_local_states,
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
        let mut bft_local_states = latest.bft_local_states.clone();
        retain_bft_states_for_validator_transition(
            &mut bft_local_states,
            next_validator_set.version(),
        );

        self.write_next_unlocked(
            Some(latest.generation),
            SnapshotContents {
                state: &latest.state,
                validator_set: &next_validator_set,
                retained_validator_sets: &retained_validator_sets,
                public_checkpoint_proof: None,
                checkpoint_floor_epoch: latest.checkpoint_floor_epoch,
                recovery_checkpoint_floors: &latest.recovery_checkpoint_floors,
                validator_safety_ready: latest.validator_safety_ready,
                minimum_signing_validator_set_version: latest.minimum_signing_validator_set_version,
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
                recovery_checkpoint_floors: &latest.recovery_checkpoint_floors,
                validator_safety_ready: latest.validator_safety_ready,
                minimum_signing_validator_set_version: latest.minimum_signing_validator_set_version,
                validator_registry: &latest.validator_registry,
                prepared_tasks: &latest.prepared_tasks,
                validator_vote_locks: &latest.validator_vote_locks,
                bft_local_states: &latest.bft_local_states,
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
                recovery_checkpoint_floors: &latest.recovery_checkpoint_floors,
                validator_safety_ready: latest.validator_safety_ready,
                minimum_signing_validator_set_version: latest.minimum_signing_validator_set_version,
                validator_registry: &latest.validator_registry,
                prepared_tasks: &latest.prepared_tasks,
                validator_vote_locks: &latest.validator_vote_locks,
                bft_local_states: &latest.bft_local_states,
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
                recovery_checkpoint_floors: &latest.recovery_checkpoint_floors,
                validator_safety_ready: latest.validator_safety_ready,
                minimum_signing_validator_set_version: latest.minimum_signing_validator_set_version,
                validator_registry: &latest.validator_registry,
                prepared_tasks: &latest.prepared_tasks,
                validator_vote_locks: &latest.validator_vote_locks,
                bft_local_states: &latest.bft_local_states,
            },
        )
    }

    pub fn next_state_recovery_checkpoint(
        &self,
    ) -> Result<StateRecoveryCheckpoint, PersistenceError> {
        let _guard = self.lock()?;
        let latest = self
            .load_unlocked()?
            .ok_or(PersistenceError::MissingSnapshot)?;
        let validator_set_version = latest.validator_set.version();
        let serial = match latest
            .recovery_checkpoint_floors
            .get(&validator_set_version)
        {
            None => 1,
            Some(floor) if !floor.certified => {
                return Err(PersistenceError::RecoveryCheckpointAwaitingFinality {
                    validator_set_version,
                    serial: floor.serial,
                });
            }
            Some(floor) => floor.serial.checked_add(1).ok_or(
                PersistenceError::RecoveryCheckpointSerialOverflow {
                    validator_set_version,
                },
            )?,
        };
        StateRecoveryCheckpoint::from_persisted(serial, &latest)
    }

    pub fn advance_recovery_checkpoint_floor(
        &self,
        checkpoint: &CertifiedStateRecoveryCheckpoint,
    ) -> Result<u64, PersistenceError> {
        let _guard = self.lock()?;
        let latest = self
            .load_unlocked()?
            .ok_or(PersistenceError::MissingSnapshot)?;

        let payload = StateRecoveryPayload::from_persisted(&latest)?;
        checkpoint.verify_payload(&payload, &latest.validator_set)?;
        let mut recovery_checkpoint_floors = latest.recovery_checkpoint_floors.clone();
        advance_recovery_checkpoint_floor_entry(
            &mut recovery_checkpoint_floors,
            checkpoint.checkpoint().validator_set_version(),
            checkpoint.checkpoint().serial(),
            checkpoint.checkpoint().digest(),
            RecoveryCheckpointFloorUpdate::Certified,
        )?;

        self.write_next_unlocked(
            Some(latest.generation),
            SnapshotContents {
                state: &latest.state,
                validator_set: &latest.validator_set,
                retained_validator_sets: &latest.retained_validator_sets,
                public_checkpoint_proof: latest.public_checkpoint_proof.as_ref(),
                checkpoint_floor_epoch: latest.checkpoint_floor_epoch,
                recovery_checkpoint_floors: &recovery_checkpoint_floors,
                validator_safety_ready: latest.validator_safety_ready,
                minimum_signing_validator_set_version: latest.minimum_signing_validator_set_version,
                validator_registry: &latest.validator_registry,
                prepared_tasks: &latest.prepared_tasks,
                validator_vote_locks: &latest.validator_vote_locks,
                bft_local_states: &latest.bft_local_states,
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
        let recovery_checkpoint_floors = latest
            .as_ref()
            .map(|snapshot| snapshot.recovery_checkpoint_floors.clone())
            .unwrap_or_default();
        let bft_local_states = latest
            .as_ref()
            .map(|snapshot| snapshot.bft_local_states.clone())
            .unwrap_or_default();

        self.write_next_unlocked(
            latest.as_ref().map(|snapshot| snapshot.generation),
            SnapshotContents {
                state,
                validator_set,
                retained_validator_sets: &retained_validator_sets,
                public_checkpoint_proof: checkpoint.as_ref(),
                checkpoint_floor_epoch,
                recovery_checkpoint_floors: &recovery_checkpoint_floors,
                validator_safety_ready: latest
                    .as_ref()
                    .map(|snapshot| snapshot.validator_safety_ready)
                    .unwrap_or(true),
                minimum_signing_validator_set_version: latest
                    .as_ref()
                    .map(|snapshot| snapshot.minimum_signing_validator_set_version)
                    .unwrap_or(validator_set.version()),
                validator_registry: &registry,
                prepared_tasks,
                validator_vote_locks: &vote_locks,
                bft_local_states: &bft_local_states,
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
        let mut bft_local_states = latest.bft_local_states.clone();
        retain_active_prepared_bft_states(&mut bft_local_states, prepared_tasks);

        self.write_next_unlocked(
            Some(latest.generation),
            SnapshotContents {
                state,
                validator_set: &latest.validator_set,
                retained_validator_sets: &retained_validator_sets,
                public_checkpoint_proof: checkpoint,
                checkpoint_floor_epoch: latest.checkpoint_floor_epoch,
                recovery_checkpoint_floors: &latest.recovery_checkpoint_floors,
                validator_safety_ready: latest.validator_safety_ready,
                minimum_signing_validator_set_version: latest.minimum_signing_validator_set_version,
                validator_registry: &latest.validator_registry,
                prepared_tasks,
                validator_vote_locks: &latest.validator_vote_locks,
                bft_local_states: &bft_local_states,
            },
        )
    }

    pub(crate) fn advance_prepared_task_phase(
        &self,
        task_id: &TaskId,
        expected_plan_digest: [u8; 32],
        phase: PreparedTaskPhase,
    ) -> Result<u64, PersistenceError> {
        let _guard = self.lock()?;
        let mut latest = self
            .load_unlocked()?
            .ok_or(PersistenceError::MissingSnapshot)?;

        let prepared = latest
            .prepared_tasks
            .get_mut(task_id)
            .ok_or(PersistenceError::StalePreparedTasks)?;
        let actual_plan_digest = prepared
            .plan_digest()
            .map_err(|_| PersistenceError::InvalidSnapshot)?;
        if actual_plan_digest != expected_plan_digest {
            return Err(PersistenceError::StalePreparedTasks);
        }
        if phase <= prepared.phase {
            return Ok(latest.generation);
        }
        prepared.advance_phase(phase);

        self.write_next_unlocked(
            Some(latest.generation),
            SnapshotContents {
                state: &latest.state,
                validator_set: &latest.validator_set,
                retained_validator_sets: &latest.retained_validator_sets,
                public_checkpoint_proof: latest.public_checkpoint_proof.as_ref(),
                checkpoint_floor_epoch: latest.checkpoint_floor_epoch,
                recovery_checkpoint_floors: &latest.recovery_checkpoint_floors,
                validator_safety_ready: latest.validator_safety_ready,
                minimum_signing_validator_set_version: latest.minimum_signing_validator_set_version,
                validator_registry: &latest.validator_registry,
                prepared_tasks: &latest.prepared_tasks,
                validator_vote_locks: &latest.validator_vote_locks,
                bft_local_states: &latest.bft_local_states,
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
        let mut bft_local_states = latest.bft_local_states.clone();
        retain_active_prepared_bft_states(&mut bft_local_states, prepared_tasks);

        self.write_next_unlocked(
            Some(latest.generation),
            SnapshotContents {
                state: &latest.state,
                validator_set: &latest.validator_set,
                retained_validator_sets: &retained_validator_sets,
                public_checkpoint_proof: latest.public_checkpoint_proof.as_ref(),
                checkpoint_floor_epoch: latest.checkpoint_floor_epoch,
                recovery_checkpoint_floors: &latest.recovery_checkpoint_floors,
                validator_safety_ready: latest.validator_safety_ready,
                minimum_signing_validator_set_version: latest.minimum_signing_validator_set_version,
                validator_registry: &latest.validator_registry,
                prepared_tasks,
                validator_vote_locks: &latest.validator_vote_locks,
                bft_local_states: &bft_local_states,
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

        if !latest.validator_safety_ready {
            return Err(PersistenceError::ValidatorSafetyStateUnavailable);
        }
        if validator_set.version() < latest.minimum_signing_validator_set_version {
            return Err(PersistenceError::SigningFenceViolation {
                minimum_validator_set_version: latest.minimum_signing_validator_set_version,
                actual_validator_set_version: validator_set.version(),
            });
        }
        validate_vote_validator_set(&latest, &scope, validator_set)?;

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

        latest
            .validator_vote_locks
            .insert((validator_id, scope.clone()), digest);
        latest.bft_local_states.remove(&(validator_id, scope));

        self.write_next_unlocked(
            Some(latest.generation),
            SnapshotContents {
                state: &latest.state,
                validator_set: &latest.validator_set,
                retained_validator_sets: &latest.retained_validator_sets,
                public_checkpoint_proof: latest.public_checkpoint_proof.as_ref(),
                checkpoint_floor_epoch: latest.checkpoint_floor_epoch,
                recovery_checkpoint_floors: &latest.recovery_checkpoint_floors,
                validator_safety_ready: latest.validator_safety_ready,
                minimum_signing_validator_set_version: latest.minimum_signing_validator_set_version,
                validator_registry: &latest.validator_registry,
                prepared_tasks: &latest.prepared_tasks,
                validator_vote_locks: &latest.validator_vote_locks,
                bft_local_states: &latest.bft_local_states,
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
        load_latest(&self.base_path)
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

pub(super) fn validate_vote_validator_set(
    snapshot: &PersistedNodeState,
    scope: &ConsensusScope,
    validator_set: &ValidatorSet,
) -> Result<(), PersistenceError> {
    match scope {
        ConsensusScope::PreparedTask(task_id) => {
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
        ConsensusScope::PublicCheckpoint { .. }
        | ConsensusScope::ValidatorSetTransition { .. }
        | ConsensusScope::StateRecoveryCheckpoint { .. } => {
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

#[derive(Clone, Copy)]
enum RecoveryCheckpointFloorUpdate {
    Vote,
    Certified,
}

fn advance_recovery_checkpoint_floor_entry(
    recovery_checkpoint_floors: &mut BTreeMap<u64, RecoveryCheckpointFloor>,
    validator_set_version: u64,
    serial: u64,
    checkpoint_digest: [u8; 32],
    update: RecoveryCheckpointFloorUpdate,
) -> Result<(), PersistenceError> {
    if serial == 0 {
        return Err(PersistenceError::UnexpectedRecoveryCheckpointSerial {
            validator_set_version,
            expected: 1,
            actual: 0,
        });
    }

    let Some(current) = recovery_checkpoint_floors.get_mut(&validator_set_version) else {
        if matches!(update, RecoveryCheckpointFloorUpdate::Vote) && serial != 1 {
            return Err(PersistenceError::UnexpectedRecoveryCheckpointSerial {
                validator_set_version,
                expected: 1,
                actual: serial,
            });
        }
        recovery_checkpoint_floors.insert(
            validator_set_version,
            RecoveryCheckpointFloor {
                serial,
                checkpoint_digest,
                certified: matches!(update, RecoveryCheckpointFloorUpdate::Certified),
            },
        );
        return Ok(());
    };

    if serial < current.serial {
        return Err(PersistenceError::StaleRecoveryCheckpointSerial {
            validator_set_version,
            minimum: current.serial,
            actual: serial,
        });
    }

    if serial == current.serial {
        if checkpoint_digest == current.checkpoint_digest {
            if matches!(update, RecoveryCheckpointFloorUpdate::Certified) {
                current.certified = true;
            }
            return Ok(());
        }

        if matches!(update, RecoveryCheckpointFloorUpdate::Certified) && !current.certified {
            *current = RecoveryCheckpointFloor {
                serial,
                checkpoint_digest,
                certified: true,
            };
            return Ok(());
        }

        return Err(PersistenceError::RecoveryCheckpointFloorConflict {
            validator_set_version,
            serial,
            locked_digest: current.checkpoint_digest,
            attempted_digest: checkpoint_digest,
        });
    }

    if matches!(update, RecoveryCheckpointFloorUpdate::Vote) {
        if !current.certified {
            return Err(PersistenceError::RecoveryCheckpointAwaitingFinality {
                validator_set_version,
                serial: current.serial,
            });
        }
        let expected = current.serial.checked_add(1).ok_or(
            PersistenceError::RecoveryCheckpointSerialOverflow {
                validator_set_version,
            },
        )?;
        if serial != expected {
            return Err(PersistenceError::UnexpectedRecoveryCheckpointSerial {
                validator_set_version,
                expected,
                actual: serial,
            });
        }
    }

    *current = RecoveryCheckpointFloor {
        serial,
        checkpoint_digest,
        certified: matches!(update, RecoveryCheckpointFloorUpdate::Certified),
    };
    Ok(())
}

fn checkpoint_matches(
    proof: &PublicCurrencyCheckpointProof,
    state: &SecondState,
    validator_set: &ValidatorSet,
) -> bool {
    proof.checkpoint().summary() == &state.public_currency_summary()
        && proof.validator_set_version() == validator_set.version()
}
