use sha2::{Digest, Sha256};

use crate::persistence::{decode_shared_recovery_state, encode_shared_recovery_state};
use crate::{
    CURRENT_PROTOCOL_VERSION, FinalityCertificate, FinalityError, FinalityStatement,
    PersistedNodeState, PersistenceError, SecondState, ValidatorId, ValidatorRegistry,
    ValidatorSet, ValidatorVote,
};

const SHARED_RECOVERY_STATE_DOMAIN: &[u8] = b"SECOND_SHARED_RECOVERY_STATE_V1\0";
const STATE_RECOVERY_CHECKPOINT_DOMAIN: &[u8] = b"SECOND_STATE_RECOVERY_CHECKPOINT_V1\0";
const RECOVERY_PROOF_FIXED_SIZE: usize = 60;
const RECOVERY_VOTE_ENCODED_SIZE: usize = 72;

#[derive(Clone)]
pub struct StateRecoveryPayload {
    state: SecondState,
    validator_set: ValidatorSet,
    validator_registry: ValidatorRegistry,
}

impl StateRecoveryPayload {
    pub(crate) fn from_shared_parts(
        state: &SecondState,
        validator_set: &ValidatorSet,
        validator_registry: &ValidatorRegistry,
    ) -> Result<Self, PersistenceError> {
        validator_registry
            .validate_current_set(validator_set)
            .map_err(|_| PersistenceError::ValidatorRegistryMismatch)?;
        Ok(Self {
            state: state.clone(),
            validator_set: validator_set.clone(),
            validator_registry: validator_registry.clone(),
        })
    }

    pub fn from_persisted(snapshot: &PersistedNodeState) -> Result<Self, PersistenceError> {
        Self::from_shared_parts(
            &snapshot.state,
            &snapshot.validator_set,
            &snapshot.validator_registry,
        )
    }

    pub fn state(&self) -> &SecondState {
        &self.state
    }

    pub fn validator_set(&self) -> &ValidatorSet {
        &self.validator_set
    }

    pub fn validator_registry(&self) -> &ValidatorRegistry {
        &self.validator_registry
    }

    pub fn encode_bytes(&self) -> Result<Vec<u8>, PersistenceError> {
        encode_shared_recovery_state(&self.state, &self.validator_set, &self.validator_registry)
    }

    pub fn decode_bytes(bytes: &[u8]) -> Result<Self, PersistenceError> {
        let (state, validator_set, validator_registry) = decode_shared_recovery_state(bytes)?;
        Ok(Self {
            state,
            validator_set,
            validator_registry,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StateRecoveryProofCodecError {
    LengthOverflow,
    InvalidLength,
}

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
        let payload = StateRecoveryPayload::from_persisted(snapshot)?;
        Self::from_payload(serial, &payload)
    }

    pub fn from_payload(
        serial: u64,
        payload: &StateRecoveryPayload,
    ) -> Result<Self, PersistenceError> {
        let encoded = payload.encode_bytes()?;
        let mut hasher = Sha256::new();
        hasher.update(SHARED_RECOVERY_STATE_DOMAIN);
        hasher.update(encoded);
        let shared_state_digest = hasher.finalize().into();

        Ok(Self {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            serial,
            validator_set_version: payload.validator_set.version(),
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
        let payload = StateRecoveryPayload::from_persisted(snapshot)?;
        self.matches_payload(&payload)
    }

    pub(crate) fn matches_payload(
        &self,
        payload: &StateRecoveryPayload,
    ) -> Result<bool, PersistenceError> {
        if self.protocol_version != CURRENT_PROTOCOL_VERSION
            || self.validator_set_version != payload.validator_set.version()
        {
            return Ok(false);
        }
        Ok(
            Self::from_payload(self.serial, payload)?.shared_state_digest
                == self.shared_state_digest,
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateRecoveryCheckpointProof {
    checkpoint: StateRecoveryCheckpoint,
    votes: Vec<ValidatorVote>,
}

impl StateRecoveryCheckpointProof {
    pub fn new(checkpoint: StateRecoveryCheckpoint, votes: Vec<ValidatorVote>) -> Self {
        Self { checkpoint, votes }
    }

    pub fn checkpoint(&self) -> &StateRecoveryCheckpoint {
        &self.checkpoint
    }

    pub fn votes(&self) -> &[ValidatorVote] {
        &self.votes
    }

    pub fn verify_checkpoint(
        self,
        validator_set: &ValidatorSet,
    ) -> Result<CertifiedStateRecoveryCheckpoint, FinalityError> {
        CertifiedStateRecoveryCheckpoint::new(self.checkpoint, self.votes, validator_set)
    }

    pub(crate) fn encode_bytes(&self) -> Result<Vec<u8>, StateRecoveryProofCodecError> {
        let encoded_len = self
            .votes
            .len()
            .checked_mul(RECOVERY_VOTE_ENCODED_SIZE)
            .and_then(|len| RECOVERY_PROOF_FIXED_SIZE.checked_add(len))
            .ok_or(StateRecoveryProofCodecError::LengthOverflow)?;
        let vote_count = u64::try_from(self.votes.len())
            .map_err(|_| StateRecoveryProofCodecError::LengthOverflow)?;

        let mut bytes = Vec::with_capacity(encoded_len);
        bytes.extend_from_slice(&self.checkpoint.protocol_version.to_be_bytes());
        bytes.extend_from_slice(&self.checkpoint.serial.to_be_bytes());
        bytes.extend_from_slice(&self.checkpoint.validator_set_version.to_be_bytes());
        bytes.extend_from_slice(&self.checkpoint.shared_state_digest);
        bytes.extend_from_slice(&vote_count.to_be_bytes());
        for vote in &self.votes {
            bytes.extend_from_slice(&vote.validator_id().value().to_be_bytes());
            bytes.extend_from_slice(&vote.signature_bytes());
        }
        Ok(bytes)
    }

    pub(crate) fn decode_bytes(bytes: &[u8]) -> Result<Self, StateRecoveryProofCodecError> {
        if bytes.len() < RECOVERY_PROOF_FIXED_SIZE {
            return Err(StateRecoveryProofCodecError::InvalidLength);
        }

        let protocol_version = u32::from_be_bytes(
            bytes[0..4]
                .try_into()
                .map_err(|_| StateRecoveryProofCodecError::InvalidLength)?,
        );
        let serial = u64::from_be_bytes(
            bytes[4..12]
                .try_into()
                .map_err(|_| StateRecoveryProofCodecError::InvalidLength)?,
        );
        let validator_set_version = u64::from_be_bytes(
            bytes[12..20]
                .try_into()
                .map_err(|_| StateRecoveryProofCodecError::InvalidLength)?,
        );
        let shared_state_digest = bytes[20..52]
            .try_into()
            .map_err(|_| StateRecoveryProofCodecError::InvalidLength)?;
        let vote_count = usize::try_from(u64::from_be_bytes(
            bytes[52..60]
                .try_into()
                .map_err(|_| StateRecoveryProofCodecError::InvalidLength)?,
        ))
        .map_err(|_| StateRecoveryProofCodecError::LengthOverflow)?;
        let expected_len = vote_count
            .checked_mul(RECOVERY_VOTE_ENCODED_SIZE)
            .and_then(|len| RECOVERY_PROOF_FIXED_SIZE.checked_add(len))
            .ok_or(StateRecoveryProofCodecError::LengthOverflow)?;
        if bytes.len() != expected_len {
            return Err(StateRecoveryProofCodecError::InvalidLength);
        }

        let mut votes = Vec::with_capacity(vote_count);
        let mut offset = RECOVERY_PROOF_FIXED_SIZE;
        for _ in 0..vote_count {
            let validator_id = ValidatorId::new(u64::from_be_bytes(
                bytes[offset..offset + 8]
                    .try_into()
                    .map_err(|_| StateRecoveryProofCodecError::InvalidLength)?,
            ));
            offset += 8;
            let signature = bytes[offset..offset + 64]
                .try_into()
                .map_err(|_| StateRecoveryProofCodecError::InvalidLength)?;
            offset += 64;
            votes.push(ValidatorVote::from_untrusted_parts(validator_id, signature));
        }

        Ok(Self::new(
            StateRecoveryCheckpoint {
                protocol_version,
                serial,
                validator_set_version,
                shared_state_digest,
            },
            votes,
        ))
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

    pub fn to_unverified_proof(&self) -> StateRecoveryCheckpointProof {
        StateRecoveryCheckpointProof::new(
            self.checkpoint.clone(),
            self.certificate.votes().to_vec(),
        )
    }

    pub fn verify_payload(
        &self,
        payload: &StateRecoveryPayload,
        validator_set: &ValidatorSet,
    ) -> Result<(), PersistenceError> {
        self.certificate
            .verify(validator_set)
            .map_err(PersistenceError::RecoveryCheckpointFinality)?;
        if payload.validator_set() != validator_set {
            return Err(PersistenceError::RecoveryValidatorSetMismatch);
        }
        if !self.checkpoint.matches_payload(payload)? {
            return Err(PersistenceError::RecoveryCheckpointDoesNotMatchState);
        }
        Ok(())
    }
}
