use crate::{
    BftProposalSubject, CertifiedPublicCurrencyCheckpoint, CertifiedStateRecoveryCheckpoint,
    CertifiedValidatorSetTransition, FinalityError, FinalityStatement, PersistenceError,
    PublicCurrencyCheckpoint, StateRecoveryCheckpoint, StateStore, ValidatorSet,
    ValidatorSetTransition, ValidatorSigner, ValidatorSigningError, ValidatorTransitionError,
    ValidatorVote,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ValidatorConsensusTarget {
    PublicCheckpoint(PublicCurrencyCheckpoint),
    ValidatorSetTransition(ValidatorSetTransition),
    StateRecoveryCheckpoint(StateRecoveryCheckpoint),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CertifiedConsensusTarget {
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
    pub(crate) fn proposal_subject(
        &self,
        store: &StateStore,
    ) -> Result<BftProposalSubject, ConsensusTargetError> {
        Ok(match self {
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

    pub(crate) fn certify(
        &self,
        votes: Vec<ValidatorVote>,
        validator_set: &ValidatorSet,
    ) -> Result<CertifiedConsensusTarget, ConsensusTargetError> {
        match self {
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
