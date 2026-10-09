use std::collections::BTreeMap;

use crate::prepared_plan::{PreparedTask, PreparedTaskPhase};
use crate::{
    BftError, BftLocalState, BftPhase, BftProposalSubject, BftQuorumCertificate, BftValue,
    CURRENT_PROTOCOL_VERSION, ConsensusScope, PersistenceError, PublicCurrencyCheckpoint,
    StateRecoveryCheckpoint, TaskId, ValidatorId, ValidatorSet, ValidatorSetTransition,
};

use super::PersistedNodeState;
use super::codec::SnapshotContents;
use super::snapshot_validation::resolve_validator_set;
use super::store::StateStore;
use super::store_recovery::next_recovery_checkpoint_serial;

impl StateStore {
    /// Called only with a QC verified against the driver's exact set.
    /// Snapshot loading still verifies persisted proofs independently.
    pub(crate) fn remember_verified_bft_prevote_qc(
        &self,
        validator_id: ValidatorId,
        certificate: &BftQuorumCertificate,
        validator_set: &ValidatorSet,
    ) -> Result<(), PersistenceError> {
        let statement = certificate.statement();
        if statement.phase() != BftPhase::Prevote
            || !matches!(statement.value(), BftValue::Digest(_))
        {
            return Err(PersistenceError::BftInvalidUnlockProof);
        }
        let _guard = self.lock()?;
        let mut latest = self
            .load_unlocked()?
            .ok_or(PersistenceError::MissingSnapshot)?;
        validate_vote_validator_set(&latest, statement.scope(), validator_set)?;
        if !validator_set.contains(validator_id) {
            return Err(PersistenceError::Bft(BftError::UnknownValidator(
                validator_id,
            )));
        }
        let state = latest
            .bft_local_states
            .get_mut(&(validator_id, statement.scope().clone()))
            .ok_or(PersistenceError::BftInvalidUnlockProof)?;
        validate_bft_state_version(state, validator_set)?;
        if statement.round() > state.round() {
            return Err(PersistenceError::BftRoundMismatch {
                current: state.round(),
                attempted: statement.round(),
            });
        }
        if state.remember_prevote_qc(certificate) {
            self.write_local_metadata_unlocked(&latest)?;
        }
        Ok(())
    }

    pub fn prepared_bft_proposal_subject(
        &self,
        task_id: TaskId,
    ) -> Result<BftProposalSubject, PersistenceError> {
        let snapshot = self
            .load_shared()?
            .ok_or(PersistenceError::MissingSnapshot)?;
        let prepared = snapshot
            .prepared_tasks
            .get(&task_id)
            .ok_or(PersistenceError::StalePreparedTasks)?;
        let digest = if prepared.commit_authorized && !prepared.conflict_abort {
            prepared
                .plan_digest()
                .map_err(|_| PersistenceError::InvalidSnapshot)?
        } else {
            self.prepared_abort_statement(&task_id)?.subject_digest()
        };
        let validator_set = self.validator_set_for_prepared_task(&task_id, digest)?;
        Ok(BftProposalSubject::new(
            validator_set.version(),
            ConsensusScope::PreparedTask(task_id),
            digest,
        ))
    }

    pub fn public_checkpoint_bft_proposal_subject(
        &self,
        checkpoint: &PublicCurrencyCheckpoint,
    ) -> Result<BftProposalSubject, PersistenceError> {
        let snapshot = self
            .load_shared()?
            .ok_or(PersistenceError::MissingSnapshot)?;
        if checkpoint.protocol_version() != CURRENT_PROTOCOL_VERSION {
            return Err(PersistenceError::Bft(BftError::WrongProtocolVersion {
                expected: CURRENT_PROTOCOL_VERSION,
                actual: checkpoint.protocol_version(),
            }));
        }
        if checkpoint.epoch() < snapshot.checkpoint_floor_epoch {
            return Err(PersistenceError::StaleCheckpointEpoch {
                minimum: snapshot.checkpoint_floor_epoch,
                actual: checkpoint.epoch(),
            });
        }
        if checkpoint.summary() != &snapshot.state.public_currency_summary() {
            return Err(PersistenceError::CheckpointDoesNotMatchState);
        }
        Ok(BftProposalSubject::new(
            snapshot.validator_set.version(),
            ConsensusScope::PublicCheckpoint {
                validator_set_version: snapshot.validator_set.version(),
                epoch: checkpoint.epoch(),
            },
            checkpoint.digest(),
        ))
    }

    pub fn validator_transition_bft_proposal_subject(
        &self,
        transition: &ValidatorSetTransition,
    ) -> Result<BftProposalSubject, PersistenceError> {
        let snapshot = self
            .load_shared()?
            .ok_or(PersistenceError::MissingSnapshot)?;
        governance_subject(
            &snapshot,
            &crate::runtime_consensus_target::ValidatorConsensusTarget::ValidatorSetTransition(
                transition.clone(),
            ),
        )
    }

    pub fn state_recovery_bft_proposal_subject(
        &self,
        checkpoint: &StateRecoveryCheckpoint,
    ) -> Result<BftProposalSubject, PersistenceError> {
        let snapshot = self
            .load_shared()?
            .ok_or(PersistenceError::MissingSnapshot)?;
        governance_subject(
            &snapshot,
            &crate::runtime_consensus_target::ValidatorConsensusTarget::StateRecoveryCheckpoint(
                checkpoint.clone(),
            ),
        )
    }

    pub fn bft_local_state(
        &self,
        validator_id: ValidatorId,
        scope: &ConsensusScope,
    ) -> Result<Option<BftLocalState>, PersistenceError> {
        Ok(self.load_shared()?.and_then(|snapshot| {
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
        self.write_local_metadata_unlocked(&latest)
    }

    pub(crate) fn catch_up_bft_round(
        &self,
        validator_id: ValidatorId,
        scope: &ConsensusScope,
        target_round: u64,
        validator_set: &ValidatorSet,
    ) -> Result<u64, PersistenceError> {
        let _guard = self.lock()?;
        let mut latest = self
            .load_unlocked()?
            .ok_or(PersistenceError::MissingSnapshot)?;
        validate_vote_validator_set(&latest, scope, validator_set)?;
        if !validator_set.contains(validator_id) {
            return Err(PersistenceError::Bft(BftError::UnknownValidator(
                validator_id,
            )));
        }
        open_prepared_voting(&mut latest, scope)?;

        let state = latest
            .bft_local_states
            .entry((validator_id, scope.clone()))
            .or_insert_with(|| BftLocalState::new(validator_set.version()));
        validate_bft_state_version(state, validator_set)?;
        if target_round <= state.round() {
            return Err(PersistenceError::BftRoundMustAdvance {
                current: state.round(),
                attempted: target_round,
            });
        }
        state.set_round(target_round);
        self.write_local_metadata_unlocked(&latest)
    }

    pub fn accept_bft_nil_precommit_qc(
        &self,
        validator_id: ValidatorId,
        certificate: &BftQuorumCertificate,
        validator_set: &ValidatorSet,
    ) -> Result<u64, PersistenceError> {
        certificate
            .verify(validator_set)
            .map_err(PersistenceError::Bft)?;
        let statement = certificate.statement();
        if statement.phase() != BftPhase::Precommit || statement.value() != BftValue::Nil {
            return Err(PersistenceError::BftInvalidUnlockProof);
        }

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
        validate_bft_round(state, statement.round())?;
        let next_round =
            statement
                .round()
                .checked_add(1)
                .ok_or(PersistenceError::BftRoundOverflow {
                    current: statement.round(),
                })?;
        state.set_round(next_round);
        self.write_local_metadata_unlocked(&latest)
    }

    pub(crate) fn lock_bft_prevote(
        &self,
        validator_id: ValidatorId,
        scope: ConsensusScope,
        round: u64,
        value: BftValue,
        validator_set: &ValidatorSet,
        unlock_certificate: Option<&BftQuorumCertificate>,
    ) -> Result<(), PersistenceError> {
        let _guard = self.lock()?;
        let mut latest = self
            .load_unlocked()?
            .ok_or(PersistenceError::MissingSnapshot)?;
        validate_bft_signing_context(&latest, validator_id, &scope, validator_set)?;
        validate_bft_vote_value(&latest, &scope, value, validator_set)?;
        open_prepared_voting(&mut latest, &scope)?;

        let state = latest
            .bft_local_states
            .entry((validator_id, scope))
            .or_insert_with(|| BftLocalState::new(validator_set.version()));
        validate_bft_state_version(state, validator_set)?;
        validate_bft_round(state, round)?;

        let unlock_round = unlock_certificate.map(|certificate| certificate.statement().round());
        if let Some(existing) = state.prevote() {
            if existing == value {
                if let Some(certificate) = unlock_certificate
                    && state.remember_prevote_qc(certificate)
                {
                    self.write_local_metadata_unlocked(&latest)?;
                }
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

        if let Some(certificate) = unlock_certificate {
            state.remember_prevote_qc(certificate);
        }
        state.set_prevote(value);
        self.write_local_metadata_unlocked(&latest)?;
        Ok(())
    }

    pub(crate) fn lock_bft_precommit(
        &self,
        validator_id: ValidatorId,
        scope: ConsensusScope,
        round: u64,
        value: BftValue,
        validator_set: &ValidatorSet,
        prevote_certificate: Option<&BftQuorumCertificate>,
    ) -> Result<(), PersistenceError> {
        let _guard = self.lock()?;
        let mut latest = self
            .load_unlocked()?
            .ok_or(PersistenceError::MissingSnapshot)?;
        validate_bft_signing_context(&latest, validator_id, &scope, validator_set)?;
        validate_bft_vote_value(&latest, &scope, value, validator_set)?;
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

        if matches!(value, BftValue::Digest(_)) && prevote_certificate.is_none() {
            return Err(PersistenceError::BftPrevoteCertificateRequired);
        }

        if let Some(certificate) = prevote_certificate {
            state.remember_prevote_qc(certificate);
        }
        state.set_precommit(value);
        self.write_local_metadata_unlocked(&latest)?;
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
        let key = (validator_id, statement.scope().clone());
        if latest
            .validator_vote_locks
            .get(&key)
            .is_some_and(|locked| *locked != digest)
            || latest
                .bft_local_states
                .get(&key)
                .and_then(|state| state.finality_ready_digest())
                .is_some_and(|ready| ready != digest)
        {
            return Err(PersistenceError::BftFinalityNotReady);
        }
        let opens_voting = matches!(statement.scope(), ConsensusScope::PreparedTask(task_id)
            if latest.prepared_tasks[task_id].phase == PreparedTaskPhase::Prepared);
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
        // A repeated proof has no new signing state to make durable.
        if !opens_voting
            && state.round() >= statement.round()
            && state.finality_ready_round() == Some(statement.round())
            && state.finality_ready_digest() == Some(digest)
            && state.finality_qc().is_some()
        {
            return Ok(latest.generation);
        }
        if statement.round() > state.round() {
            state.set_round(statement.round());
        }
        state.mark_finality_ready(statement.round(), digest);
        state.remember_finality_qc(certificate);
        self.write_local_metadata_unlocked(&latest)
    }

    pub fn bft_finality_ready(
        &self,
        validator_id: ValidatorId,
        scope: &ConsensusScope,
        digest: [u8; 32],
    ) -> Result<bool, PersistenceError> {
        Ok(self.load_shared()?.is_some_and(|snapshot| {
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

    pub(super) fn write_local_metadata_unlocked(
        &self,
        latest: &crate::PersistedNodeState,
    ) -> Result<u64, PersistenceError> {
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
                pending_public_changes: latest.pending_public_changes.as_ref(),
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

pub(super) fn open_prepared_voting(
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

fn validate_bft_vote_value(
    snapshot: &PersistedNodeState,
    scope: &ConsensusScope,
    value: BftValue,
    validators: &ValidatorSet,
) -> Result<(), PersistenceError> {
    if let BftValue::Digest(digest) = value
        && let Some(transition) =
            snapshot
                .pending_governance
                .values()
                .find_map(|pending| match pending {
                    super::PendingGovernance::CollectingTransition(transition)
                        if &transition.scope() == scope
                            && (transition.digest() == digest
                                || transition.clone().with_handoff_digest(None).digest()
                                    == digest) =>
                    {
                        Some(transition)
                    }
                    _ => None,
                })
    {
        return Err(PersistenceError::TransitionCollectionIncomplete {
            validator_set_version: transition.current_validator_set_version(),
            currency_frontier: transition.currency_frontier(),
        });
    }
    if let ConsensusScope::PreparedTask(task_id) = scope
        && let Some(plan) = snapshot.prepared_tasks.get(task_id)
        && let BftValue::Digest(digest) = value
        && digest
            != crate::task_abort::statement(task_id, plan.request_digest, validators)
                .subject_digest()
        && !plan
            .candidate(digest)
            .map_err(|_| PersistenceError::InvalidSnapshot)?
            .is_some_and(|candidate| candidate.commit_authorized)
    {
        return Err(PersistenceError::StalePreparedTasks);
    }
    Ok(())
}

pub(super) fn validate_bft_signing_context(
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
    validate_vote_validator_set(snapshot, scope, validator_set)?;
    if let ConsensusScope::PreparedTask(task_id) = scope {
        let plan = snapshot
            .prepared_tasks
            .get(task_id)
            .ok_or(PersistenceError::StalePreparedTasks)?;
        let contains = |handoff: &super::TaskHandoff| {
            handoff.task_context(task_id).is_some_and(|context| {
                context.validator_set_version == plan.validator_set_version
                    && context.request_digest == plan.request_digest
            })
        };
        // Closing ownership alone does not close Abort signing for a late
        // unowned witness. Every sealed candidate must cover the request.
        let outside_sealed = snapshot.pending_governance.values().any(|pending| {
            matches!(pending, super::PendingGovernance::Transition(transition)
                if transition.current_validator_set_version() == snapshot.validator_set.version()
                    && transition.handoff.as_ref().is_none_or(|handoff| !contains(handoff)))
        });
        let outside_installed = plan.validator_set_version < snapshot.validator_set.version()
            && snapshot
                .state
                .protocol
                .task_handoff
                .as_ref()
                .is_none_or(|handoff| !contains(handoff));
        if outside_sealed || outside_installed {
            return Err(PersistenceError::TaskAdmissionClosed {
                validator_set_version: snapshot.validator_set.version(),
            });
        }
    }
    Ok(())
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
        ConsensusScope::CurrencyAllocation { .. }
        | ConsensusScope::PublicCheckpoint { .. }
        | ConsensusScope::StateRecoveryCheckpoint { .. } => {
            snapshot
                .validator_registry
                .validate_current_set(validator_set)
                .map_err(|_| PersistenceError::ValidatorRegistryMismatch)?;
        }
    }
    Ok(())
}

pub(super) fn governance_subject(
    snapshot: &PersistedNodeState,
    target: &crate::runtime_consensus_target::ValidatorConsensusTarget,
) -> Result<BftProposalSubject, PersistenceError> {
    use crate::runtime_consensus_target::ValidatorConsensusTarget;
    let version = snapshot.validator_set.version();
    let actual_version = match target {
        ValidatorConsensusTarget::ValidatorSetTransition(value) => {
            value.current_validator_set_version()
        }
        ValidatorConsensusTarget::StateRecoveryCheckpoint(value) => value.validator_set_version(),
        _ => return Err(PersistenceError::InvalidSnapshot),
    };
    if actual_version != version {
        return Err(PersistenceError::Bft(BftError::WrongValidatorSetVersion {
            expected: version,
            actual: actual_version,
        }));
    }
    let (_, scope, digest) = match target {
        ValidatorConsensusTarget::ValidatorSetTransition(transition) => {
            let already_admitted = matches!(
                snapshot.pending_governance.get(&transition.digest()),
                Some(super::PendingGovernance::Transition(value)) if value == transition
            );
            if !already_admitted
                && snapshot.pending_governance.values().any(|pending| {
                    matches!(pending, super::PendingGovernance::CollectingTransition(value)
                    if value.scope() == transition.scope())
                })
            {
                return Err(PersistenceError::TransitionCollectionIncomplete {
                    validator_set_version: version,
                    currency_frontier: transition.currency_frontier(),
                });
            }
            if snapshot.state.next_currency_address() != transition.currency_frontier() {
                return Err(PersistenceError::StaleState);
            }
            let hydrated = super::task_handoff::hydrate(snapshot, transition)?;
            if hydrated.handoff_digest != transition.handoff_digest {
                return Err(PersistenceError::StalePreparedTasks);
            }
            snapshot
                .validator_registry
                .validate_transition(&snapshot.validator_set, transition.next_validator_set())
                .map_err(|_| PersistenceError::ValidatorRegistryMismatch)?;
            (
                transition.current_validator_set_version(),
                transition.scope(),
                transition.digest(),
            )
        }
        ValidatorConsensusTarget::StateRecoveryCheckpoint(checkpoint) => {
            let resumed = snapshot
                .pending_governance
                .contains_key(&checkpoint.digest())
                && snapshot
                    .recovery_checkpoint_floors
                    .get(&version)
                    .is_some_and(|floor| {
                        !floor.certified
                            && floor.serial == checkpoint.serial()
                            && floor.checkpoint_digest == checkpoint.digest()
                    });
            if !resumed {
                let expected = next_recovery_checkpoint_serial(snapshot)?;
                if checkpoint.serial() != expected {
                    return Err(PersistenceError::UnexpectedRecoveryCheckpointSerial {
                        validator_set_version: version,
                        expected,
                        actual: checkpoint.serial(),
                    });
                }
            }
            if !checkpoint.matches_admitted_or_persisted(snapshot)? {
                return Err(PersistenceError::RecoveryCheckpointDoesNotMatchState);
            }
            (
                checkpoint.validator_set_version(),
                ConsensusScope::StateRecoveryCheckpoint {
                    validator_set_version: checkpoint.validator_set_version(),
                    serial: checkpoint.serial(),
                },
                checkpoint.digest(),
            )
        }
        _ => return Err(PersistenceError::InvalidSnapshot),
    };
    Ok(BftProposalSubject::new(version, scope, digest))
}
