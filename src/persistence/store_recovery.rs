use std::collections::BTreeMap;

use super::codec::SnapshotContents;
use super::store::StateStore;
use super::{PersistedNodeState, RecoveryCheckpointFloor};
use crate::{
    CertifiedStateRecoveryCheckpoint, CertifiedValidatorSetTransition, PersistenceError,
    SecondState, StateRecoveryCheckpoint, StateRecoveryCheckpointProof, StateRecoveryPayload,
    ValidatorId, ValidatorRegistry, ValidatorSet, ValidatorTransitionError,
};

impl StateStore {
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

        self.write_initial_shared_state_unlocked(payload, Some(checkpoint.to_unverified_proof()))
    }

    /// Caller holds the store lock and has authenticated the shared state.
    pub(super) fn write_initial_shared_state_unlocked(
        &self,
        payload: &StateRecoveryPayload,
        recovery_checkpoint_proof: Option<StateRecoveryCheckpointProof>,
    ) -> Result<u64, PersistenceError> {
        let retained_validator_sets = payload.retained_validator_sets().clone();
        let recovery_checkpoint_floors = recovery_checkpoint_proof
            .iter()
            .map(|proof| {
                (
                    proof.checkpoint().validator_set_version(),
                    RecoveryCheckpointFloor {
                        serial: proof.checkpoint().serial(),
                        checkpoint_digest: proof.checkpoint().digest(),
                        certified: true,
                    },
                )
            })
            .collect();
        let public_checkpoint_states = None;
        let validator_transition_proofs = payload.validator_transition_proofs.clone();
        let prepared_tasks = BTreeMap::new();
        let validator_vote_locks = BTreeMap::new();
        let bft_local_states = BTreeMap::new();
        self.write_next_unlocked(
            None,
            SnapshotContents {
                task_receipts: &BTreeMap::new(),
                pending_governance: None,
                state: payload.state(),
                validator_set: payload.validator_set(),
                retained_validator_sets: &retained_validator_sets,
                public_checkpoint_proof: None,
                public_checkpoint_baseline: None,
                latest_public_delta: None,
                public_checkpoint_states: public_checkpoint_states.as_ref(),
                validator_transition_proofs: &validator_transition_proofs,
                recovery_checkpoint_proof: recovery_checkpoint_proof.as_ref(),
                checkpoint_floor_epoch: 0,
                recovery_checkpoint_floors: &recovery_checkpoint_floors,
                validator_safety_ready: false,
                minimum_signing_validator_set_version: payload.validator_set().version(),
                pending_validator_safety_recovery: None,
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
                validator_safety_ready: true,
                minimum_signing_validator_set_version: latest.validator_set.version(),
                pending_validator_safety_recovery: None,
                validator_registry: &latest.validator_registry,
                prepared_tasks: &latest.prepared_tasks,
                validator_vote_locks: &latest.validator_vote_locks,
                bft_local_states: &latest.bft_local_states,
            },
        )
    }

    pub(crate) fn try_complete_pending_validator_safety_recovery(
        &self,
        validator_id: ValidatorId,
        new_consensus_public_key: [u8; 32],
    ) -> Result<Option<u64>, PersistenceError> {
        let _guard = self.lock()?;
        let latest = self
            .load_unlocked()?
            .ok_or(PersistenceError::MissingSnapshot)?;

        if latest.validator_safety_ready {
            return Ok(None);
        }
        let Some(pending) = latest.pending_validator_safety_recovery.as_ref() else {
            return Ok(None);
        };
        if pending.validator_id != validator_id {
            return Err(PersistenceError::InvalidValidatorSafetyRecovery);
        }
        pending.validate_current(&latest.validator_set, &latest.validator_transition_proofs)?;

        let current = latest
            .validator_set
            .validator(validator_id)
            .ok_or(PersistenceError::InvalidValidatorSafetyRecovery)?;
        if current.consensus_public_key() != new_consensus_public_key {
            return Err(PersistenceError::InvalidValidatorSafetyRecovery);
        }

        let recovery_proof = latest
            .recovery_checkpoint_proof
            .as_ref()
            .ok_or(PersistenceError::InvalidValidatorSafetyRecovery)?;
        if recovery_proof.checkpoint().validator_set_version() != latest.validator_set.version() {
            return Err(PersistenceError::InvalidValidatorSafetyRecovery);
        }
        let recovery_floor = latest
            .recovery_checkpoint_floors
            .get(&latest.validator_set.version())
            .ok_or(PersistenceError::InvalidValidatorSafetyRecovery)?;
        if !recovery_floor.certified
            || recovery_floor.serial != recovery_proof.checkpoint().serial()
            || recovery_floor.checkpoint_digest != recovery_proof.checkpoint().digest()
        {
            return Err(PersistenceError::InvalidValidatorSafetyRecovery);
        }

        let generation = self.write_next_unlocked(
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
                validator_safety_ready: true,
                minimum_signing_validator_set_version: latest.validator_set.version(),
                pending_validator_safety_recovery: None,
                validator_registry: &latest.validator_registry,
                prepared_tasks: &latest.prepared_tasks,
                validator_vote_locks: &latest.validator_vote_locks,
                bft_local_states: &latest.bft_local_states,
            },
        )?;
        Ok(Some(generation))
    }

    pub fn next_state_recovery_checkpoint(
        &self,
    ) -> Result<StateRecoveryCheckpoint, PersistenceError> {
        let _guard = self.lock()?;
        let latest = self
            .load_unlocked()?
            .ok_or(PersistenceError::MissingSnapshot)?;
        if let Some(floor) = latest
            .recovery_checkpoint_floors
            .get(&latest.validator_set.version())
            && !floor.certified
            && let Some(super::PendingGovernance::Recovery(checkpoint)) =
                latest.pending_governance.get(&floor.checkpoint_digest)
        {
            return Ok(checkpoint.clone());
        }
        let serial = next_recovery_checkpoint_serial(&latest)?;
        StateRecoveryCheckpoint::from_persisted(serial, &latest)
    }

    pub fn advance_recovery_checkpoint_floor(
        &self,
        checkpoint: &CertifiedStateRecoveryCheckpoint,
    ) -> Result<u64, PersistenceError> {
        self.install_recovery_checkpoint_inner(checkpoint, false)
    }

    pub(crate) fn install_recovery_checkpoint_evidence(
        &self,
        checkpoint: &CertifiedStateRecoveryCheckpoint,
    ) -> Result<u64, PersistenceError> {
        self.install_recovery_checkpoint_inner(checkpoint, true)
    }

    fn install_recovery_checkpoint_inner(
        &self,
        checkpoint: &CertifiedStateRecoveryCheckpoint,
        allow_historical: bool,
    ) -> Result<u64, PersistenceError> {
        let _guard = self.lock()?;
        let latest = self
            .load_unlocked()?
            .ok_or(PersistenceError::MissingSnapshot)?;

        checkpoint
            .certificate()
            .verify(&latest.validator_set)
            .map_err(PersistenceError::RecoveryCheckpointFinality)?;
        if allow_historical
            && let Some(floor) = latest
                .recovery_checkpoint_floors
                .get(&latest.validator_set.version())
        {
            if checkpoint.checkpoint().serial() < floor.serial {
                return Err(PersistenceError::StaleRecoveryCheckpointSerial {
                    validator_set_version: latest.validator_set.version(),
                    minimum: floor.serial,
                    actual: checkpoint.checkpoint().serial(),
                });
            }
            if floor.certified
                && floor.serial == checkpoint.checkpoint().serial()
                && floor.checkpoint_digest == checkpoint.checkpoint().digest()
            {
                return Ok(latest.generation);
            }
        }
        let matches = checkpoint.checkpoint().matches_persisted(&latest)?;
        if !matches && !allow_historical {
            return Err(PersistenceError::RecoveryCheckpointDoesNotMatchState);
        }
        if latest
            .recovery_checkpoint_floors
            .get(&latest.validator_set.version())
            .is_some_and(|floor| {
                floor.certified
                    && floor.serial == checkpoint.checkpoint().serial()
                    && floor.checkpoint_digest == checkpoint.checkpoint().digest()
            })
        {
            return Ok(latest.generation);
        }
        let mut recovery_checkpoint_floors = latest.recovery_checkpoint_floors.clone();
        advance_recovery_checkpoint_floor_entry(
            &mut recovery_checkpoint_floors,
            checkpoint.checkpoint().validator_set_version(),
            checkpoint.checkpoint().serial(),
            checkpoint.checkpoint().digest(),
            RecoveryCheckpointFloorUpdate::Certified,
        )?;
        let recovery_checkpoint_proof = matches.then(|| checkpoint.to_unverified_proof());
        let mut pending_governance = latest.pending_governance.clone();
        pending_governance.retain(|_, pending| !matches!(pending, super::PendingGovernance::Recovery(value) if value.serial() <= checkpoint.checkpoint().serial()));
        if !matches {
            let serial = checkpoint.checkpoint().serial().checked_add(1).ok_or(
                PersistenceError::RecoveryCheckpointSerialOverflow {
                    validator_set_version: latest.validator_set.version(),
                },
            )?;
            let successor = StateRecoveryCheckpoint::from_persisted(serial, &latest)?;
            pending_governance.insert(
                successor.digest(),
                super::PendingGovernance::Recovery(successor),
            );
        }

        self.write_next_unlocked(
            Some(latest.generation),
            SnapshotContents {
                task_receipts: &latest.task_receipts,
                pending_governance: Some(&pending_governance),
                state: &latest.state,
                validator_set: &latest.validator_set,
                retained_validator_sets: &latest.retained_validator_sets,
                public_checkpoint_proof: latest.public_checkpoint_proof.as_ref(),
                public_checkpoint_baseline: latest.public_checkpoint_baseline.as_ref(),
                latest_public_delta: latest.latest_public_delta.as_ref(),
                public_checkpoint_states: latest.public_checkpoint_states.as_ref(),
                validator_transition_proofs: &latest.validator_transition_proofs,
                recovery_checkpoint_proof: recovery_checkpoint_proof.as_ref(),
                checkpoint_floor_epoch: latest.checkpoint_floor_epoch,
                recovery_checkpoint_floors: &recovery_checkpoint_floors,
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

pub(super) fn next_recovery_checkpoint_serial(
    snapshot: &PersistedNodeState,
) -> Result<u64, PersistenceError> {
    let validator_set_version = snapshot.validator_set.version();
    match snapshot
        .recovery_checkpoint_floors
        .get(&validator_set_version)
    {
        None => Ok(1),
        Some(floor) if !floor.certified => {
            Err(PersistenceError::RecoveryCheckpointAwaitingFinality {
                validator_set_version,
                serial: floor.serial,
            })
        }
        Some(floor) => {
            floor
                .serial
                .checked_add(1)
                .ok_or(PersistenceError::RecoveryCheckpointSerialOverflow {
                    validator_set_version,
                })
        }
    }
}

pub(super) fn recovery_checkpoint_matches(
    proof: &StateRecoveryCheckpointProof,
    state: &SecondState,
    validator_set: &ValidatorSet,
    validator_registry: &ValidatorRegistry,
    retained_validator_sets: &BTreeMap<u64, ValidatorSet>,
) -> bool {
    let Ok(certified) = proof.clone().verify_checkpoint(validator_set) else {
        return false;
    };
    let Ok(payload) = StateRecoveryPayload::from_shared_parts(
        state,
        validator_set,
        validator_registry,
        retained_validator_sets,
    ) else {
        return false;
    };
    certified.verify_payload(&payload, validator_set).is_ok()
}

#[derive(Clone, Copy)]
pub(super) enum RecoveryCheckpointFloorUpdate {
    Vote,
    Certified,
}

pub(super) fn advance_recovery_checkpoint_floor_entry(
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
