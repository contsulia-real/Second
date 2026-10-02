use ed25519_dalek::SigningKey;

use crate::persistence::VoteLockStatus;
use crate::{
    BftError, BftPhase, BftProposal, BftProposalSubject, BftQuorumCertificate, BftStatement,
    BftValue, BftVote, CURRENT_PROTOCOL_VERSION, ConsensusScope, FinalityError, FinalityStatement,
    PersistenceError, PublicCurrencyCheckpoint, StateRecoveryCheckpoint, StateStore, TaskId,
    ValidatorId, ValidatorSet, ValidatorSetTransition, ValidatorVote,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValidatorSigningError {
    Persistence(PersistenceError),
    Bft(BftError),
    Finality(FinalityError),
    ConsensusSigningKeyMismatch(ValidatorId),
    LocalSafetyStateUnavailable,
    VoteLocked {
        validator_id: ValidatorId,
        locked_digest: [u8; 32],
        attempted_digest: [u8; 32],
    },
}

impl From<PersistenceError> for ValidatorSigningError {
    fn from(value: PersistenceError) -> Self {
        Self::Persistence(value)
    }
}

impl From<BftError> for ValidatorSigningError {
    fn from(value: BftError) -> Self {
        Self::Bft(value)
    }
}

impl From<FinalityError> for ValidatorSigningError {
    fn from(value: FinalityError) -> Self {
        Self::Finality(value)
    }
}

#[derive(Clone)]
pub struct ValidatorSigner {
    validator_id: ValidatorId,
    signing_key: SigningKey,
    store: StateStore,
}

impl ValidatorSigner {
    pub fn new(validator_id: ValidatorId, signing_key: SigningKey, store: StateStore) -> Self {
        Self {
            validator_id,
            signing_key,
            store,
        }
    }

    pub const fn validator_id(&self) -> ValidatorId {
        self.validator_id
    }
    pub(crate) fn consensus_public_key(&self) -> [u8; 32] {
        self.signing_key.verifying_key().to_bytes()
    }

    pub fn sign_public_checkpoint(
        &self,
        checkpoint: &PublicCurrencyCheckpoint,
        validator_set: &ValidatorSet,
    ) -> Result<ValidatorVote, ValidatorSigningError> {
        let statement = checkpoint.finality_statement(validator_set.version());
        self.sign_locked(
            &statement,
            validator_set,
            ConsensusScope::PublicCheckpoint {
                validator_set_version: validator_set.version(),
                epoch: checkpoint.epoch(),
            },
            |latest| {
                if checkpoint.epoch() < latest.checkpoint_floor_epoch {
                    return Err(PersistenceError::StaleCheckpointEpoch {
                        minimum: latest.checkpoint_floor_epoch,
                        actual: checkpoint.epoch(),
                    });
                }
                if checkpoint.summary() != &latest.state.public_currency_summary() {
                    return Err(PersistenceError::CheckpointDoesNotMatchState);
                }
                Ok(())
            },
        )
    }

    pub fn sign_state_recovery_checkpoint(
        &self,
        checkpoint: &StateRecoveryCheckpoint,
        validator_set: &ValidatorSet,
    ) -> Result<ValidatorVote, ValidatorSigningError> {
        self.sign_locked(
            &checkpoint.finality_statement(),
            validator_set,
            ConsensusScope::StateRecoveryCheckpoint {
                validator_set_version: checkpoint.validator_set_version(),
                serial: checkpoint.serial(),
            },
            |latest| {
                if !checkpoint.matches_persisted(latest)? {
                    return Err(PersistenceError::RecoveryCheckpointDoesNotMatchState);
                }
                Ok(())
            },
        )
    }

    pub fn sign_validator_set_transition(
        &self,
        transition: &ValidatorSetTransition,
        validator_set: &ValidatorSet,
    ) -> Result<ValidatorVote, ValidatorSigningError> {
        self.sign_locked(
            &transition.finality_statement(),
            validator_set,
            ConsensusScope::ValidatorSetTransition {
                current_validator_set_version: transition.current_validator_set_version(),
            },
            |latest| {
                latest
                    .validator_registry
                    .validate_transition(&latest.validator_set, transition.next_validator_set())
                    .map_err(|_| PersistenceError::ValidatorRegistryMismatch)
            },
        )
    }

    pub fn complete_safety_recovery(
        &self,
        previous_validator_set: &ValidatorSet,
        certified_transition: &crate::CertifiedValidatorSetTransition,
    ) -> Result<u64, ValidatorSigningError> {
        self.store
            .complete_validator_safety_recovery(
                self.validator_id,
                self.signing_key.verifying_key().to_bytes(),
                previous_validator_set,
                certified_transition,
            )
            .map_err(ValidatorSigningError::from)
    }

    pub fn sign_bft_proposal(
        &self,
        subject: &BftProposalSubject,
        round: u64,
        validator_set: &ValidatorSet,
    ) -> Result<BftProposal, ValidatorSigningError> {
        self.validate_consensus_key(validator_set)?;
        Ok(BftProposal::sign(
            subject,
            round,
            self.validator_id,
            &self.signing_key,
            validator_set,
        )?)
    }

    pub fn sign_bft_prevote(
        &self,
        scope: ConsensusScope,
        round: u64,
        value: BftValue,
        validator_set: &ValidatorSet,
        unlock_certificate: Option<&BftQuorumCertificate>,
    ) -> Result<BftVote, ValidatorSigningError> {
        self.validate_consensus_key(validator_set)?;
        self.validate_bft_scope(&scope, validator_set)?;

        let unlock_round = if let Some(certificate) = unlock_certificate {
            certificate.verify(validator_set)?;
            let statement = certificate.statement();
            if statement.phase() != BftPhase::Prevote
                || statement.scope() != &scope
                || statement.value() != value
                || statement.round() >= round
            {
                return Err(PersistenceError::BftInvalidUnlockProof.into());
            }
            Some(statement.round())
        } else {
            None
        };

        self.store.lock_bft_prevote(
            self.validator_id,
            scope.clone(),
            round,
            value,
            validator_set,
            unlock_round,
        )?;

        let statement = BftStatement::new(
            validator_set.version(),
            scope,
            round,
            BftPhase::Prevote,
            value,
        );
        Ok(BftVote::sign_unchecked(
            &statement,
            self.validator_id,
            &self.signing_key,
        ))
    }

    pub fn sign_bft_precommit(
        &self,
        scope: ConsensusScope,
        round: u64,
        value: BftValue,
        validator_set: &ValidatorSet,
        prevote_certificate: Option<&BftQuorumCertificate>,
    ) -> Result<BftVote, ValidatorSigningError> {
        self.validate_consensus_key(validator_set)?;
        self.validate_bft_scope(&scope, validator_set)?;

        let has_prevote_qc = if let BftValue::Digest(_) = value {
            let certificate =
                prevote_certificate.ok_or(PersistenceError::BftPrevoteCertificateRequired)?;
            certificate.verify(validator_set)?;
            let statement = certificate.statement();
            if statement.phase() != BftPhase::Prevote
                || statement.scope() != &scope
                || statement.round() != round
                || statement.value() != value
            {
                return Err(PersistenceError::BftPrevoteCertificateRequired.into());
            }
            true
        } else {
            false
        };

        self.store.lock_bft_precommit(
            self.validator_id,
            scope.clone(),
            round,
            value,
            validator_set,
            has_prevote_qc,
        )?;

        let statement = BftStatement::new(
            validator_set.version(),
            scope,
            round,
            BftPhase::Precommit,
            value,
        );
        Ok(BftVote::sign_unchecked(
            &statement,
            self.validator_id,
            &self.signing_key,
        ))
    }

    pub fn prepared_task_lock(
        &self,
        task_id: TaskId,
    ) -> Result<Option<[u8; 32]>, PersistenceError> {
        self.store
            .finality_vote_lock(self.validator_id, ConsensusScope::PreparedTask(task_id))
    }

    pub(crate) fn sign_prepared_task(
        &self,
        task_id: TaskId,
        statement: &FinalityStatement,
        validator_set: &ValidatorSet,
    ) -> Result<ValidatorVote, ValidatorSigningError> {
        self.sign_locked(
            statement,
            validator_set,
            ConsensusScope::PreparedTask(task_id),
            |_| Ok(()),
        )
    }

    fn validate_bft_scope(
        &self,
        scope: &ConsensusScope,
        validator_set: &ValidatorSet,
    ) -> Result<(), ValidatorSigningError> {
        if scope.matches_validator_set_version(validator_set.version()) {
            return Ok(());
        }

        Err(BftError::ScopeValidatorSetVersionMismatch {
            expected: validator_set.version(),
            actual: scope
                .explicit_validator_set_version()
                .unwrap_or(validator_set.version()),
        }
        .into())
    }

    fn validate_consensus_key(
        &self,
        validator_set: &ValidatorSet,
    ) -> Result<(), ValidatorSigningError> {
        let credential = validator_set
            .validator(self.validator_id)
            .ok_or(BftError::UnknownValidator(self.validator_id))?;
        if self.signing_key.verifying_key().to_bytes() != credential.consensus_public_key() {
            return Err(ValidatorSigningError::ConsensusSigningKeyMismatch(
                self.validator_id,
            ));
        }
        Ok(())
    }

    fn sign_locked<F>(
        &self,
        statement: &FinalityStatement,
        validator_set: &ValidatorSet,
        scope: ConsensusScope,
        validate_latest: F,
    ) -> Result<ValidatorVote, ValidatorSigningError>
    where
        F: FnOnce(&crate::PersistedNodeState) -> Result<(), PersistenceError>,
    {
        if statement.protocol_version() != CURRENT_PROTOCOL_VERSION {
            return Err(FinalityError::WrongProtocolVersion {
                expected: CURRENT_PROTOCOL_VERSION,
                actual: statement.protocol_version(),
            }
            .into());
        }

        if statement.validator_set_version() != validator_set.version() {
            return Err(FinalityError::WrongValidatorSetVersion {
                expected: validator_set.version(),
                actual: statement.validator_set_version(),
            }
            .into());
        }

        let credential = validator_set
            .validator(self.validator_id)
            .ok_or(FinalityError::UnknownValidator(self.validator_id))?;

        if self.signing_key.verifying_key().to_bytes() != credential.consensus_public_key() {
            return Err(ValidatorSigningError::ConsensusSigningKeyMismatch(
                self.validator_id,
            ));
        }

        let attempted_digest = statement.subject_digest();
        let lock_status = match self.store.lock_finality_vote(
            self.validator_id,
            scope,
            attempted_digest,
            validator_set,
            validate_latest,
        ) {
            Ok(status) => status,
            Err(PersistenceError::ValidatorSafetyStateUnavailable) => {
                return Err(ValidatorSigningError::LocalSafetyStateUnavailable);
            }
            Err(error) => return Err(error.into()),
        };

        match lock_status {
            VoteLockStatus::Inserted | VoteLockStatus::AlreadyLocked => {}
            VoteLockStatus::Conflict(locked_digest) => {
                return Err(ValidatorSigningError::VoteLocked {
                    validator_id: self.validator_id,
                    locked_digest,
                    attempted_digest,
                });
            }
        }

        Ok(ValidatorVote::sign_unchecked(
            statement,
            self.validator_id,
            &self.signing_key,
        ))
    }
}
