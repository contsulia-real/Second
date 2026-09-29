use std::collections::BTreeSet;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

use crate::{CURRENT_PROTOCOL_VERSION, FinalityError, ValidatorId, ValidatorSet};

const FINALITY_DOMAIN: &[u8] = b"SECOND_FINALITY_V1\0";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FinalityStatement {
    protocol_version: u32,
    validator_set_version: u64,
    subject_digest: [u8; 32],
}

impl FinalityStatement {
    pub const fn new(
        protocol_version: u32,
        validator_set_version: u64,
        subject_digest: [u8; 32],
    ) -> Self {
        Self {
            protocol_version,
            validator_set_version,
            subject_digest,
        }
    }

    pub const fn protocol_version(&self) -> u32 {
        self.protocol_version
    }

    pub const fn validator_set_version(&self) -> u64 {
        self.validator_set_version
    }

    pub const fn subject_digest(&self) -> [u8; 32] {
        self.subject_digest
    }

    pub fn canonical_signing_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(
            FINALITY_DOMAIN.len() + size_of::<u32>() + size_of::<u64>() + self.subject_digest.len(),
        );
        out.extend_from_slice(FINALITY_DOMAIN);
        out.extend_from_slice(&self.protocol_version.to_be_bytes());
        out.extend_from_slice(&self.validator_set_version.to_be_bytes());
        out.extend_from_slice(&self.subject_digest);
        out
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatorVote {
    validator_id: ValidatorId,
    signature: [u8; 64],
}

impl ValidatorVote {
    pub fn sign(
        statement: &FinalityStatement,
        validator_id: ValidatorId,
        signing_key: &SigningKey,
    ) -> Self {
        Self {
            validator_id,
            signature: signing_key
                .sign(&statement.canonical_signing_bytes())
                .to_bytes(),
        }
    }

    pub const fn from_parts(validator_id: ValidatorId, signature: [u8; 64]) -> Self {
        Self {
            validator_id,
            signature,
        }
    }

    pub const fn validator_id(&self) -> ValidatorId {
        self.validator_id
    }

    pub const fn signature_bytes(&self) -> [u8; 64] {
        self.signature
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FinalityCertificate {
    statement: FinalityStatement,
    votes: Vec<ValidatorVote>,
}

impl FinalityCertificate {
    pub fn new(
        statement: FinalityStatement,
        votes: Vec<ValidatorVote>,
        validator_set: &ValidatorSet,
    ) -> Result<Self, FinalityError> {
        let certificate = Self { statement, votes };
        certificate.verify(validator_set)?;
        Ok(certificate)
    }

    pub const fn statement(&self) -> FinalityStatement {
        self.statement
    }

    pub fn votes(&self) -> &[ValidatorVote] {
        &self.votes
    }

    pub fn vote_count(&self) -> usize {
        self.votes.len()
    }

    pub fn verify(&self, validator_set: &ValidatorSet) -> Result<(), FinalityError> {
        if self.statement.protocol_version() != CURRENT_PROTOCOL_VERSION {
            return Err(FinalityError::WrongProtocolVersion {
                expected: CURRENT_PROTOCOL_VERSION,
                actual: self.statement.protocol_version(),
            });
        }

        if self.statement.validator_set_version() != validator_set.version() {
            return Err(FinalityError::WrongValidatorSetVersion {
                expected: validator_set.version(),
                actual: self.statement.validator_set_version(),
            });
        }

        let message = self.statement.canonical_signing_bytes();
        let mut seen = BTreeSet::new();

        for vote in &self.votes {
            if !seen.insert(vote.validator_id()) {
                return Err(FinalityError::DuplicateVote(vote.validator_id()));
            }

            let credential = validator_set
                .credential(vote.validator_id())
                .ok_or(FinalityError::UnknownValidator(vote.validator_id()))?;

            let verifying_key = VerifyingKey::from_bytes(&credential.consensus_public_key())
                .map_err(|_| FinalityError::InvalidValidatorKey(vote.validator_id()))?;
            let signature = Signature::from_bytes(&vote.signature_bytes());

            verifying_key
                .verify_strict(&message, &signature)
                .map_err(|_| FinalityError::InvalidSignature(vote.validator_id()))?;
        }

        if seen.len() < validator_set.quorum_threshold() {
            return Err(FinalityError::InsufficientVotes {
                required: validator_set.quorum_threshold(),
                actual: seen.len(),
            });
        }

        Ok(())
    }
}
