use ed25519_dalek::SigningKey;

use crate::persistence::VoteLockStatus;
use crate::{
    CURRENT_PROTOCOL_VERSION, FinalityError, FinalityStatement, PersistenceError,
    PublicCurrencyCheckpoint, StateStore, TaskId, ValidatorId, ValidatorSet,
    ValidatorSetTransition, ValidatorVote,
};

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub(crate) enum FinalityScope {
    PreparedTask(TaskId),
    PublicCheckpoint {
        validator_set_version: u64,
        epoch: u64,
    },
    ValidatorSetTransition {
        current_validator_set_version: u64,
        activation_epoch: u64,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValidatorSigningError {
    Persistence(PersistenceError),
    Finality(FinalityError),
    ConsensusSigningKeyMismatch(ValidatorId),
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

    pub fn sign_public_checkpoint(
        &self,
        checkpoint: &PublicCurrencyCheckpoint,
        validator_set: &ValidatorSet,
    ) -> Result<ValidatorVote, ValidatorSigningError> {
        let statement = checkpoint.finality_statement(validator_set.version());
        self.sign_locked(
            &statement,
            validator_set,
            FinalityScope::PublicCheckpoint {
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

    pub fn sign_validator_set_transition(
        &self,
        transition: &ValidatorSetTransition,
        validator_set: &ValidatorSet,
    ) -> Result<ValidatorVote, ValidatorSigningError> {
        self.sign_locked(
            &transition.finality_statement(),
            validator_set,
            FinalityScope::ValidatorSetTransition {
                current_validator_set_version: transition.current_validator_set_version(),
                activation_epoch: transition.activation_epoch(),
            },
            |latest| {
                latest
                    .validator_registry
                    .validate_transition(&latest.validator_set, transition.next_validator_set())
                    .map_err(|_| PersistenceError::ValidatorRegistryMismatch)
            },
        )
    }

    pub fn prepared_task_lock(
        &self,
        task_id: TaskId,
    ) -> Result<Option<[u8; 32]>, PersistenceError> {
        self.store
            .finality_vote_lock(self.validator_id, FinalityScope::PreparedTask(task_id))
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
            FinalityScope::PreparedTask(task_id),
            |_| Ok(()),
        )
    }

    fn sign_locked<F>(
        &self,
        statement: &FinalityStatement,
        validator_set: &ValidatorSet,
        scope: FinalityScope,
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
        match self.store.lock_finality_vote(
            self.validator_id,
            scope,
            attempted_digest,
            validator_set,
            validate_latest,
        )? {
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
