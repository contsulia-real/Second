use sha2::{Digest, Sha256};

use crate::persistence::encode_shared_recovery_state;
use crate::{
    CURRENT_PROTOCOL_VERSION, FinalityCertificate, FinalityError, FinalityStatement,
    PersistedNodeState, PersistenceError, ValidatorSet, ValidatorVote,
};

const SHARED_RECOVERY_STATE_DOMAIN: &[u8] = b"SECOND_SHARED_RECOVERY_STATE_V1\0";
const STATE_RECOVERY_CHECKPOINT_DOMAIN: &[u8] = b"SECOND_STATE_RECOVERY_CHECKPOINT_V1\0";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateRecoveryCheckpoint {
    protocol_version: u32,
    serial: u64,
    validator_set_version: u64,
    shared_state_digest: [u8; 32],
}

impl StateRecoveryCheckpoint {
    pub fn from_persisted(
        serial: u64,
        snapshot: &PersistedNodeState,
    ) -> Result<Self, PersistenceError> {
        let encoded = encode_shared_recovery_state(snapshot)?;
        let mut hasher = Sha256::new();
        hasher.update(SHARED_RECOVERY_STATE_DOMAIN);
        hasher.update(encoded);
        let shared_state_digest = hasher.finalize().into();

        Ok(Self {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            serial,
            validator_set_version: snapshot.validator_set.version(),
            shared_state_digest,
        })
    }

    pub const fn protocol_version(&self) -> u32 {
        self.protocol_version
    }

    pub const fn serial(&self) -> u64 {
        self.serial
    }

    pub const fn validator_set_version(&self) -> u64 {
        self.validator_set_version
    }

    pub const fn shared_state_digest(&self) -> [u8; 32] {
        self.shared_state_digest
    }

    pub fn digest(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(STATE_RECOVERY_CHECKPOINT_DOMAIN);
        hasher.update(self.protocol_version.to_be_bytes());
        hasher.update(self.serial.to_be_bytes());
        hasher.update(self.validator_set_version.to_be_bytes());
        hasher.update(self.shared_state_digest);
        hasher.finalize().into()
    }

    pub fn finality_statement(&self) -> FinalityStatement {
        FinalityStatement::new(
            self.protocol_version,
            self.validator_set_version,
            self.digest(),
        )
    }

    pub(crate) fn matches_persisted(
        &self,
        snapshot: &PersistedNodeState,
    ) -> Result<bool, PersistenceError> {
        if self.protocol_version != CURRENT_PROTOCOL_VERSION
            || self.validator_set_version != snapshot.validator_set.version()
        {
            return Ok(false);
        }

        let current = Self::from_persisted(self.serial, snapshot)?;
        Ok(current.shared_state_digest == self.shared_state_digest)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CertifiedStateRecoveryCheckpoint {
    checkpoint: StateRecoveryCheckpoint,
    certificate: FinalityCertificate,
}

impl CertifiedStateRecoveryCheckpoint {
    pub fn new(
        checkpoint: StateRecoveryCheckpoint,
        votes: Vec<ValidatorVote>,
        validator_set: &ValidatorSet,
    ) -> Result<Self, FinalityError> {
        let certificate =
            FinalityCertificate::new(checkpoint.finality_statement(), votes, validator_set)?;
        Ok(Self {
            checkpoint,
            certificate,
        })
    }

    pub fn checkpoint(&self) -> &StateRecoveryCheckpoint {
        &self.checkpoint
    }

    pub fn certificate(&self) -> &FinalityCertificate {
        &self.certificate
    }
}
