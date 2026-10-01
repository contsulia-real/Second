use std::collections::BTreeMap;

use crate::prepared_plan::{PreparedTask, PreparedTaskPhase};
use crate::{
    BftError, BftLocalState, BftPhase, BftQuorumCertificate, BftValue, ConsensusScope,
    PersistenceError, TaskId, ValidatorId, ValidatorSet,
};

use super::codec::SnapshotContents;
use super::store::{StateStore, validate_vote_validator_set};

impl StateStore {
    pub fn bft_local_state(
        &self,
        validator_id: ValidatorId,
        scope: &ConsensusScope,
    ) -> Result<Option<BftLocalState>, PersistenceError> {
        Ok(self.load()?.and_then(|snapshot| {
            snapshot
                .bft_local_states
                .get(&(validator_id, scope.clone()))
                .cloned()
        }))
    }

    pub fn advance_bft_round(
        &self,
        validator_id: ValidatorId,
        scope: &ConsensusScope,
        next_round: u64,
    ) -> Result<u64, PersistenceError> {
        let _guard = self.lock()?;
        let mut latest = self
            .load_unlocked()?
            .ok_or(PersistenceError::MissingSnapshot)?;
        let state = latest
            .bft_local_states
            .get_mut(&(validator_id, scope.clone()))
            .ok_or(PersistenceError::BftRoundMismatch {
                current: 0,
                attempted: next_round,
            })?;
        let expected =
            state
                .round()
                .checked_add(1)
                .ok_or(PersistenceError::BftRoundMustAdvance {
                    current: state.round(),
                    attempted: next_round,
                })?;
        if next_round != expected {
            return Err(PersistenceError::BftRoundMustAdvance {
                current: state.round(),
                attempted: next_round,
            });
        }
        state.set_round(next_round);
        self.write_bft_local_states_unlocked(&latest)
    }

    pub(crate) fn lock_bft_prevote(
        &self,
        validator_id: ValidatorId,
        scope: ConsensusScope,
        round: u64,
        value: BftValue,
        validator_set: &ValidatorSet,
        unlock_round: Option<u64>,
    ) -> Result<(), PersistenceError> {
        let _guard = self.lock()?;
        let mut latest = self
            .load_unlocked()?
            .ok_or(PersistenceError::MissingSnapshot)?;
        validate_bft_signing_context(&latest, validator_id, &scope, validator_set)?;
        open_prepared_voting(&mut latest, &scope)?;

        let state = latest
            .bft_local_states
            .entry((validator_id, scope))
            .or_insert_with(|| BftLocalState::new(validator_set.version()));
        validate_bft_state_version(state, validator_set)?;
        validate_bft_round(state, round)?;

        if let Some(existing) = state.prevote() {
            if existing == value {
                return Ok(());
            }
            return Err(PersistenceError::BftVoteConflict {
                phase: BftPhase::Prevote,
                round,
                locked: existing,
                attempted: value,
            });
        }

        if let BftValue::Digest(attempted_digest) = value
            && let (Some(locked_round), Some(locked_digest)) =
                (state.locked_round(), state.locked_digest())
            && locked_digest != attempted_digest
        {
            let Some(unlock_round) = unlock_round else {
                return Err(PersistenceError::BftUnlockProofRequired {
                    locked_round,
                    attempted_digest,
                });
            };
            if unlock_round <= locked_round || unlock_round >= round {
                return Err(PersistenceError::BftInvalidUnlockProof);
            }
        }

        state.set_prevote(value);
        self.write_bft_local_states_unlocked(&latest)?;
        Ok(())
    }

    pub(crate) fn lock_bft_precommit(
        &self,
        validator_id: ValidatorId,
        scope: ConsensusScope,
        round: u64,
        value: BftValue,
        validator_set: &ValidatorSet,
        has_prevote_qc: bool,
    ) -> Result<(), PersistenceError> {
        let _guard = self.lock()?;
        let mut latest = self
            .load_unlocked()?
            .ok_or(PersistenceError::MissingSnapshot)?;
        validate_bft_signing_context(&latest, validator_id, &scope, validator_set)?;
        open_prepared_voting(&mut latest, &scope)?;

        let state = latest
            .bft_local_states
            .entry((validator_id, scope))
            .or_insert_with(|| BftLocalState::new(validator_set.version()));
        validate_bft_state_version(state, validator_set)?;
        validate_bft_round(state, round)?;

        if let Some(existing) = state.precommit() {
            if existing == value {
                return Ok(());
            }
            return Err(PersistenceError::BftVoteConflict {
                phase: BftPhase::Precommit,
                round,
                locked: existing,
                attempted: value,
            });
        }

        if matches!(value, BftValue::Digest(_)) && !has_prevote_qc {
            return Err(PersistenceError::BftPrevoteCertificateRequired);
        }

        state.set_precommit(value);
        self.write_bft_local_states_unlocked(&latest)?;
        Ok(())
    }

    pub fn accept_bft_precommit_qc(
        &self,
        validator_id: ValidatorId,
        certificate: &BftQuorumCertificate,
        validator_set: &ValidatorSet,
    ) -> Result<u64, PersistenceError> {
        certificate
            .verify(validator_set)
            .map_err(PersistenceError::Bft)?;
        let statement = certificate.statement();
        if statement.phase() != BftPhase::Precommit {
            return Err(PersistenceError::BftInvalidUnlockProof);
        }
        let digest = statement
            .value()
            .digest()
            .ok_or(PersistenceError::BftInvalidUnlockProof)?;

        let _guard = self.lock()?;
        let mut latest = self
            .load_unlocked()?
            .ok_or(PersistenceError::MissingSnapshot)?;
        validate_vote_validator_set(&latest, statement.scope(), validator_set)?;
        open_prepared_voting(&mut latest, statement.scope())?;
        if !validator_set.contains(validator_id) {
            return Err(PersistenceError::Bft(BftError::UnknownValidator(
                validator_id,
            )));
        }

        let state = latest
            .bft_local_states
            .entry((validator_id, statement.scope().clone()))
            .or_insert_with(|| BftLocalState::new(validator_set.version()));
        validate_bft_state_version(state, validator_set)?;
        if statement.round() > state.round() {
            state.set_round(statement.round());
        }
        state.mark_finality_ready(statement.round(), digest);
        self.write_bft_local_states_unlocked(&latest)
    }

    pub fn bft_finality_ready(
        &self,
        validator_id: ValidatorId,
        scope: &ConsensusScope,
        digest: [u8; 32],
    ) -> Result<bool, PersistenceError> {
        Ok(self.load()?.is_some_and(|snapshot| {
            snapshot
                .bft_local_states
                .get(&(validator_id, scope.clone()))
                .is_some_and(|state| state.finality_ready_digest() == Some(digest))
        }))
    }

    pub(crate) fn require_bft_finality_ready(
        &self,
        snapshot: &crate::PersistedNodeState,
        validator_id: ValidatorId,
        scope: &ConsensusScope,
        digest: [u8; 32],
    ) -> Result<(), PersistenceError> {
        if snapshot
            .validator_vote_locks
            .get(&(validator_id, scope.clone()))
            .is_some_and(|locked| *locked == digest)
        {
            return Ok(());
        }

        if snapshot
            .bft_local_states
            .get(&(validator_id, scope.clone()))
            .is_some_and(|state| state.finality_ready_digest() == Some(digest))
        {
            Ok(())
        } else {
            Err(PersistenceError::BftFinalityNotReady)
        }
    }

    fn write_bft_local_states_unlocked(
        &self,
        latest: &crate::PersistedNodeState,
    ) -> Result<u64, PersistenceError> {
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
}

pub(super) fn retain_bft_states_for_validator_transition(
    bft_local_states: &mut BTreeMap<(ValidatorId, ConsensusScope), BftLocalState>,
    next_validator_set_version: u64,
) {
    bft_local_states.retain(|(_, scope), state| {
        matches!(scope, ConsensusScope::PreparedTask(_))
            || state.validator_set_version() == next_validator_set_version
    });
}

pub(super) fn retain_active_prepared_bft_states(
    bft_local_states: &mut BTreeMap<(ValidatorId, ConsensusScope), BftLocalState>,
    prepared_tasks: &BTreeMap<TaskId, PreparedTask>,
) {
    bft_local_states.retain(|(_, scope), _| {
        !matches!(
            scope,
            ConsensusScope::PreparedTask(task_id) if !prepared_tasks.contains_key(task_id)
        )
    });
}

fn open_prepared_voting(
    snapshot: &mut crate::PersistedNodeState,
    scope: &ConsensusScope,
) -> Result<(), PersistenceError> {
    let ConsensusScope::PreparedTask(task_id) = scope else {
        return Ok(());
    };
    let prepared = snapshot
        .prepared_tasks
        .get_mut(task_id)
        .ok_or(PersistenceError::StalePreparedTasks)?;
    prepared.advance_phase(PreparedTaskPhase::Voting);
    Ok(())
}

fn validate_bft_signing_context(
    snapshot: &crate::PersistedNodeState,
    validator_id: ValidatorId,
    scope: &ConsensusScope,
    validator_set: &ValidatorSet,
) -> Result<(), PersistenceError> {
    if !snapshot.validator_safety_ready {
        return Err(PersistenceError::ValidatorSafetyStateUnavailable);
    }
    if validator_set.version() < snapshot.minimum_signing_validator_set_version {
        return Err(PersistenceError::SigningFenceViolation {
            minimum_validator_set_version: snapshot.minimum_signing_validator_set_version,
            actual_validator_set_version: validator_set.version(),
        });
    }
    if !validator_set.contains(validator_id) {
        return Err(PersistenceError::Bft(BftError::UnknownValidator(
            validator_id,
        )));
    }
    validate_vote_validator_set(snapshot, scope, validator_set)
}

fn validate_bft_state_version(
    state: &BftLocalState,
    validator_set: &ValidatorSet,
) -> Result<(), PersistenceError> {
    if state.validator_set_version() != validator_set.version() {
        return Err(PersistenceError::Bft(BftError::WrongValidatorSetVersion {
            expected: validator_set.version(),
            actual: state.validator_set_version(),
        }));
    }
    Ok(())
}

fn validate_bft_round(state: &BftLocalState, round: u64) -> Result<(), PersistenceError> {
    if state.round() != round {
        return Err(PersistenceError::BftRoundMismatch {
            current: state.round(),
            attempted: round,
        });
    }
    Ok(())
}
