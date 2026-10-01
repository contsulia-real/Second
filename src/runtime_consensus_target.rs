use crate::prepared_plan::PreparedTaskPhase;

use crate::{
    BftProposalSubject, CURRENT_PROTOCOL_VERSION, CertifiedPublicCurrencyCheckpoint,
    CertifiedStateRecoveryCheckpoint, CertifiedValidatorSetTransition, FinalityCertificate,
    FinalityError, FinalityStatement, PersistenceError, PublicCurrencyCheckpoint,
    StateRecoveryCheckpoint, StateStore, TaskId, ValidatorSet, ValidatorSetTransition,
    ValidatorSigner, ValidatorSigningError, ValidatorTransitionError, ValidatorVote,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ValidatorConsensusTarget {
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
        task_id: TaskId,
    ) -> Result<Self, ConsensusTargetError> {
        let subject = store.prepared_bft_proposal_subject(task_id.clone())?;
        Ok(Self::PreparedTask {
            task_id,
            plan_digest: subject.digest(),
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
            Self::PublicCheckpoint(_)
            | Self::ValidatorSetTransition(_)
            | Self::StateRecoveryCheckpoint(_) => Ok(active_validator_set.clone()),
        }
    }

    pub(crate) fn proposal_subject(
        &self,
        store: &StateStore,
    ) -> Result<BftProposalSubject, ConsensusTargetError> {
        Ok(match self {
            Self::PreparedTask {
                task_id,
                plan_digest,
            } => {
                let subject = store.prepared_bft_proposal_subject(task_id.clone())?;
                if subject.digest() != *plan_digest {
                    return Err(ConsensusTargetError::Persistence(
                        PersistenceError::StalePreparedTasks,
                    ));
                }
                subject
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
            Self::PreparedTask { task_id, .. } => signer.sign_prepared_task(
                task_id.clone(),
                &self.finality_statement(validator_set),
                validator_set,
            )?,
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

    pub(crate) fn persist_certified(&self, store: &StateStore) -> Result<(), ConsensusTargetError> {
        if let Self::PreparedTask {
            task_id,
            plan_digest,
        } = self
        {
            store.advance_prepared_task_phase(
                task_id,
                *plan_digest,
                PreparedTaskPhase::Finalized,
            )?;
        }
        Ok(())
    }

    pub(crate) fn certify(
        &self,
        votes: Vec<ValidatorVote>,
        validator_set: &ValidatorSet,
    ) -> Result<CertifiedConsensusTarget, ConsensusTargetError> {
        match self {
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
