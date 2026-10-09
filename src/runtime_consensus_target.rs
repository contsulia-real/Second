use crate::{
    BftProposalSubject, CURRENT_PROTOCOL_VERSION, CertifiedPublicCurrencyCheckpoint,
    CertifiedStateRecoveryCheckpoint, CertifiedValidatorSetTransition, FinalityCertificate,
    FinalityError, FinalityStatement, PersistenceError, PublicCurrencyCheckpoint,
    StateRecoveryCheckpoint, StateStore, TaskId, ValidatorSet, ValidatorSetTransition,
    ValidatorSigner, ValidatorSigningError, ValidatorTransitionError, ValidatorVote,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ValidatorConsensusTarget {
    CurrencyAllocation(crate::CurrencyAllocation),
    PreparedTask {
        task_id: TaskId,
        plan_digest: [u8; 32],
    },
    PublicCheckpoint(PublicCurrencyCheckpoint),
    ValidatorSetTransition(ValidatorSetTransition),
    StateRecoveryCheckpoint(StateRecoveryCheckpoint),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CertifiedConsensusTarget {
    CurrencyAllocation {
        allocation: crate::CurrencyAllocation,
        certificate: FinalityCertificate,
    },
    PreparedTask {
        task_id: TaskId,
        certificate: FinalityCertificate,
    },
    PublicCheckpoint(CertifiedPublicCurrencyCheckpoint),
    ValidatorSetTransition(CertifiedValidatorSetTransition),
    StateRecoveryCheckpoint(CertifiedStateRecoveryCheckpoint),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ConsensusTargetError {
    Persistence(PersistenceError),
    Signing(ValidatorSigningError),
    Finality(FinalityError),
    ValidatorTransition(ValidatorTransitionError),
}

impl From<PersistenceError> for ConsensusTargetError {
    fn from(value: PersistenceError) -> Self {
        Self::Persistence(value)
    }
}

impl From<ValidatorSigningError> for ConsensusTargetError {
    fn from(value: ValidatorSigningError) -> Self {
        Self::Signing(value)
    }
}

impl ValidatorConsensusTarget {
    pub(crate) fn prepared_task(
        store: &StateStore,
        validator_id: crate::ValidatorId,
        task_id: TaskId,
    ) -> Result<Self, ConsensusTargetError> {
        let subject = store.prepared_bft_proposal_subject(task_id.clone())?;
        let snapshot = store
            .load_shared()?
            .ok_or(PersistenceError::MissingSnapshot)?;
        let key = (validator_id, subject.scope().clone());
        let local = snapshot.bft_local_states.get(&key);
        let locked_finality = snapshot.validator_vote_locks.get(&key).copied();
        let finality_ready = local.and_then(|state| state.finality_ready_digest());
        if let (Some(locked), Some(ready)) = (locked_finality, finality_ready)
            && locked != ready
        {
            return Err(PersistenceError::InvalidSnapshot.into());
        }
        // A certified precommit decision or immutable finality vote outranks
        // every earlier prevote lock, regardless of which outcome it names.
        if let Some(digest) = locked_finality.or(finality_ready) {
            store.validator_set_for_prepared_task(&task_id, digest)?;
            return Ok(Self::PreparedTask {
                task_id,
                plan_digest: digest,
            });
        }
        let mut digest = subject.digest();
        if let Some(local) = local
            && let Some(locked) = local
                .valid_prevote_qc()
                .filter(|qc| {
                    local
                        .locked_round()
                        .is_none_or(|round| qc.statement().round() >= round)
                })
                .and_then(|qc| match qc.statement().value() {
                    crate::BftValue::Digest(value) => Some(value),
                    _ => None,
                })
                .or_else(|| local.locked_digest())
        {
            store.validator_set_for_prepared_task(&task_id, locked)?;
            digest = locked;
        }
        Ok(Self::PreparedTask {
            task_id,
            plan_digest: digest,
        })
    }

    pub(crate) fn validator_set(
        &self,
        store: &StateStore,
        active_validator_set: &ValidatorSet,
    ) -> Result<ValidatorSet, ConsensusTargetError> {
        match self {
            Self::PreparedTask {
                task_id,
                plan_digest,
            } => Ok(store.validator_set_for_prepared_task(task_id, *plan_digest)?),
            Self::CurrencyAllocation(_)
            | Self::PublicCheckpoint(_)
            | Self::ValidatorSetTransition(_)
            | Self::StateRecoveryCheckpoint(_) => Ok(active_validator_set.clone()),
        }
    }

    pub(crate) fn proposal_subject(
        &self,
        store: &StateStore,
    ) -> Result<BftProposalSubject, ConsensusTargetError> {
        Ok(match self {
            Self::CurrencyAllocation(allocation) => {
                store.validate_currency_allocation(allocation)?;
                allocation.subject()
            }
            Self::PreparedTask {
                task_id,
                plan_digest,
            } => {
                let validators = store.validator_set_for_prepared_task(task_id, *plan_digest)?;
                BftProposalSubject::new(
                    validators.version(),
                    crate::ConsensusScope::PreparedTask(task_id.clone()),
                    *plan_digest,
                )
            }
            Self::PublicCheckpoint(checkpoint) => {
                store.public_checkpoint_bft_proposal_subject(checkpoint)?
            }
            Self::ValidatorSetTransition(transition) => {
                store.validator_transition_bft_proposal_subject(transition)?
            }
            Self::StateRecoveryCheckpoint(checkpoint) => {
                store.state_recovery_bft_proposal_subject(checkpoint)?
            }
        })
    }

    pub(crate) fn finality_statement(&self, validator_set: &ValidatorSet) -> FinalityStatement {
        match self {
            Self::CurrencyAllocation(allocation) => allocation.finality_statement(),
            Self::PreparedTask { plan_digest, .. } => FinalityStatement::new(
                CURRENT_PROTOCOL_VERSION,
                validator_set.version(),
                *plan_digest,
            ),
            Self::PublicCheckpoint(checkpoint) => {
                checkpoint.finality_statement(validator_set.version())
            }
            Self::ValidatorSetTransition(transition) => transition.finality_statement(),
            Self::StateRecoveryCheckpoint(checkpoint) => checkpoint.finality_statement(),
        }
    }

    pub(crate) fn sign_vote(
        &self,
        signer: &ValidatorSigner,
        validator_set: &ValidatorSet,
    ) -> Result<ValidatorVote, ConsensusTargetError> {
        Ok(match self {
            Self::CurrencyAllocation(allocation) => {
                signer.sign_currency_allocation(allocation, validator_set)?
            }
            Self::PreparedTask { task_id, .. } => {
                let statement = self.finality_statement(validator_set);
                if statement == signer.store().prepared_abort_statement(task_id)? {
                    signer.sign_prepared_abort(task_id.clone(), &statement, validator_set)?
                } else {
                    signer.sign_prepared_task(task_id.clone(), &statement, validator_set)?
                }
            }
            Self::PublicCheckpoint(checkpoint) => {
                signer.sign_public_checkpoint(checkpoint, validator_set)?
            }
            Self::ValidatorSetTransition(transition) => {
                signer.sign_validator_set_transition(transition, validator_set)?
            }
            Self::StateRecoveryCheckpoint(checkpoint) => {
                signer.sign_state_recovery_checkpoint(checkpoint, validator_set)?
            }
        })
    }

    pub(crate) fn persist_certified(
        &self,
        store: &StateStore,
        certificate: &FinalityCertificate,
    ) -> Result<(), ConsensusTargetError> {
        if let Self::CurrencyAllocation(allocation) = self {
            store.install_currency_allocation(allocation, certificate)?;
        }
        if let Self::PreparedTask {
            task_id,
            plan_digest,
        } = self
        {
            if store.prepared_abort_statement(task_id)? == certificate.statement() {
                store.install_prepared_abort(task_id, certificate)?;
            } else {
                store.finalize_prepared_task(task_id, *plan_digest, certificate)?;
            }
        }
        Ok(())
    }

    pub(crate) fn certify(
        &self,
        votes: Vec<ValidatorVote>,
        validator_set: &ValidatorSet,
    ) -> Result<CertifiedConsensusTarget, ConsensusTargetError> {
        match self {
            Self::CurrencyAllocation(allocation) => {
                FinalityCertificate::new(allocation.finality_statement(), votes, validator_set)
                    .map(|certificate| CertifiedConsensusTarget::CurrencyAllocation {
                        allocation: allocation.clone(),
                        certificate,
                    })
                    .map_err(ConsensusTargetError::Finality)
            }
            Self::PreparedTask { task_id, .. } => FinalityCertificate::new(
                self.finality_statement(validator_set),
                votes,
                validator_set,
            )
            .map(|certificate| CertifiedConsensusTarget::PreparedTask {
                task_id: task_id.clone(),
                certificate,
            })
            .map_err(ConsensusTargetError::Finality),
            Self::PublicCheckpoint(checkpoint) => {
                CertifiedPublicCurrencyCheckpoint::new(checkpoint.clone(), votes, validator_set)
                    .map(CertifiedConsensusTarget::PublicCheckpoint)
                    .map_err(ConsensusTargetError::Finality)
            }
            Self::ValidatorSetTransition(transition) => {
                CertifiedValidatorSetTransition::new(transition.clone(), votes, validator_set)
                    .map(CertifiedConsensusTarget::ValidatorSetTransition)
                    .map_err(ConsensusTargetError::ValidatorTransition)
            }
            Self::StateRecoveryCheckpoint(checkpoint) => {
                CertifiedStateRecoveryCheckpoint::new(checkpoint.clone(), votes, validator_set)
                    .map(CertifiedConsensusTarget::StateRecoveryCheckpoint)
                    .map_err(ConsensusTargetError::Finality)
            }
        }
    }
}
