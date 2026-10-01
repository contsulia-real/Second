use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

use crate::{BftError, CURRENT_PROTOCOL_VERSION, ConsensusScope, ValidatorId, ValidatorSet};

const BFT_PROPOSAL_DOMAIN: &[u8] = b"SECOND_BFT_PROPOSAL_V1\0";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BftProposalSubject {
    validator_set_version: u64,
    scope: ConsensusScope,
    digest: [u8; 32],
}

impl BftProposalSubject {
    pub(crate) fn new(validator_set_version: u64, scope: ConsensusScope, digest: [u8; 32]) -> Self {
        Self {
            validator_set_version,
            scope,
            digest,
        }
    }

    pub const fn validator_set_version(&self) -> u64 {
        self.validator_set_version
    }

    pub fn scope(&self) -> &ConsensusScope {
        &self.scope
    }

    pub const fn digest(&self) -> [u8; 32] {
        self.digest
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BftProposal {
    protocol_version: u32,
    validator_set_version: u64,
    scope: ConsensusScope,
    round: u64,
    proposer_id: ValidatorId,
    subject_digest: [u8; 32],
    signature: [u8; 64],
}

impl BftProposal {
    pub(crate) fn sign(
        subject: &BftProposalSubject,
        round: u64,
        proposer_id: ValidatorId,
        signing_key: &SigningKey,
        validator_set: &ValidatorSet,
    ) -> Result<Self, BftError> {
        validate_subject(subject, validator_set)?;
        let expected = validator_set.proposer(round);
        if proposer_id != expected {
            return Err(BftError::WrongProposer {
                expected,
                actual: proposer_id,
            });
        }
        let credential = validator_set
            .validator(proposer_id)
            .ok_or(BftError::UnknownValidator(proposer_id))?;
        if credential.consensus_public_key() != signing_key.verifying_key().to_bytes() {
            return Err(BftError::InvalidSignature(proposer_id));
        }

        let mut proposal = Self {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            validator_set_version: subject.validator_set_version,
            scope: subject.scope.clone(),
            round,
            proposer_id,
            subject_digest: subject.digest,
            signature: [0; 64],
        };
        proposal.signature = signing_key
            .sign(&proposal.canonical_signing_bytes())
            .to_bytes();
        Ok(proposal)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn from_untrusted_parts(
        protocol_version: u32,
        validator_set_version: u64,
        scope: ConsensusScope,
        round: u64,
        proposer_id: ValidatorId,
        subject_digest: [u8; 32],
        signature: [u8; 64],
    ) -> Self {
        Self {
            protocol_version,
            validator_set_version,
            scope,
            round,
            proposer_id,
            subject_digest,
            signature,
        }
    }

    pub const fn protocol_version(&self) -> u32 {
        self.protocol_version
    }

    pub const fn validator_set_version(&self) -> u64 {
        self.validator_set_version
    }

    pub fn scope(&self) -> &ConsensusScope {
        &self.scope
    }

    pub const fn round(&self) -> u64 {
        self.round
    }

    pub const fn proposer_id(&self) -> ValidatorId {
        self.proposer_id
    }

    pub const fn subject_digest(&self) -> [u8; 32] {
        self.subject_digest
    }

    pub const fn signature_bytes(&self) -> [u8; 64] {
        self.signature
    }

    pub fn canonical_signing_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(BFT_PROPOSAL_DOMAIN.len() + 4 + 8 + 32 + 8 + 8 + 32);
        out.extend_from_slice(BFT_PROPOSAL_DOMAIN);
        out.extend_from_slice(&self.protocol_version.to_be_bytes());
        out.extend_from_slice(&self.validator_set_version.to_be_bytes());
        self.scope.encode_canonical(&mut out);
        out.extend_from_slice(&self.round.to_be_bytes());
        out.extend_from_slice(&self.proposer_id.value().to_be_bytes());
        out.extend_from_slice(&self.subject_digest);
        out
    }

    pub fn verify(&self, validator_set: &ValidatorSet) -> Result<(), BftError> {
        if self.protocol_version != CURRENT_PROTOCOL_VERSION {
            return Err(BftError::WrongProtocolVersion {
                expected: CURRENT_PROTOCOL_VERSION,
                actual: self.protocol_version,
            });
        }
        if self.validator_set_version != validator_set.version() {
            return Err(BftError::WrongValidatorSetVersion {
                expected: validator_set.version(),
                actual: self.validator_set_version,
            });
        }
        if let Some(actual) = self.scope.explicit_validator_set_version()
            && actual != self.validator_set_version
        {
            return Err(BftError::ScopeValidatorSetVersionMismatch {
                expected: self.validator_set_version,
                actual,
            });
        }
        let expected = validator_set.proposer(self.round);
        if self.proposer_id != expected {
            return Err(BftError::WrongProposer {
                expected,
                actual: self.proposer_id,
            });
        }
        let credential = validator_set
            .validator(self.proposer_id)
            .ok_or(BftError::UnknownValidator(self.proposer_id))?;
        let key = VerifyingKey::from_bytes(&credential.consensus_public_key())
            .map_err(|_| BftError::InvalidValidatorKey(self.proposer_id))?;
        key.verify_strict(
            &self.canonical_signing_bytes(),
            &Signature::from_bytes(&self.signature),
        )
        .map_err(|_| BftError::InvalidSignature(self.proposer_id))
    }

    pub fn matches_subject(&self, subject: &BftProposalSubject) -> bool {
        self.validator_set_version == subject.validator_set_version
            && self.scope == subject.scope
            && self.subject_digest == subject.digest
    }
}

fn validate_subject(
    subject: &BftProposalSubject,
    validator_set: &ValidatorSet,
) -> Result<(), BftError> {
    if subject.validator_set_version != validator_set.version() {
        return Err(BftError::WrongValidatorSetVersion {
            expected: validator_set.version(),
            actual: subject.validator_set_version,
        });
    }
    if let Some(actual) = subject.scope.explicit_validator_set_version()
        && actual != subject.validator_set_version
    {
        return Err(BftError::ScopeValidatorSetVersionMismatch {
            expected: subject.validator_set_version,
            actual,
        });
    }
    Ok(())
}
