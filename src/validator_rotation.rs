use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

use crate::{CURRENT_PROTOCOL_VERSION, ValidatorCredential, ValidatorId, ValidatorRotationError};

const ROTATION_DOMAIN: &[u8] = b"SECOND_VALIDATOR_CONSENSUS_ROTATION_V1\0";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidatorRotationAuthority {
    Identity,
    Recovery,
}

impl ValidatorRotationAuthority {
    pub const fn tag(self) -> u8 {
        match self {
            Self::Identity => 1,
            Self::Recovery => 2,
        }
    }

    pub const fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(Self::Identity),
            2 => Some(Self::Recovery),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatorConsensusKeyRotationRequest {
    protocol_version: u32,
    authority: ValidatorRotationAuthority,
    validator_id: ValidatorId,
    current_validator_set_version: u64,
    new_consensus_public_key: [u8; 32],
    signature: [u8; 64],
}

impl ValidatorConsensusKeyRotationRequest {
    pub fn sign(
        protocol_version: u32,
        authority: ValidatorRotationAuthority,
        validator_id: ValidatorId,
        current_validator_set_version: u64,
        new_consensus_public_key: [u8; 32],
        signing_key: &SigningKey,
    ) -> Result<Self, ValidatorRotationError> {
        VerifyingKey::from_bytes(&new_consensus_public_key)
            .map_err(|_| ValidatorRotationError::InvalidNewConsensusKey(validator_id))?;

        let message = canonical_rotation_bytes(
            protocol_version,
            authority,
            validator_id,
            current_validator_set_version,
            new_consensus_public_key,
        );

        Ok(Self {
            protocol_version,
            authority,
            validator_id,
            current_validator_set_version,
            new_consensus_public_key,
            signature: signing_key.sign(&message).to_bytes(),
        })
    }

    pub fn from_untrusted_parts(
        protocol_version: u32,
        authority: ValidatorRotationAuthority,
        validator_id: ValidatorId,
        current_validator_set_version: u64,
        new_consensus_public_key: [u8; 32],
        signature: [u8; 64],
    ) -> Self {
        Self {
            protocol_version,
            authority,
            validator_id,
            current_validator_set_version,
            new_consensus_public_key,
            signature,
        }
    }

    pub fn encode_bytes(&self) -> [u8; 117] {
        let mut bytes = [0_u8; 117];
        bytes[0..4].copy_from_slice(&self.protocol_version.to_be_bytes());
        bytes[4] = self.authority.tag();
        bytes[5..13].copy_from_slice(&self.validator_id.value().to_be_bytes());
        bytes[13..21].copy_from_slice(&self.current_validator_set_version.to_be_bytes());
        bytes[21..53].copy_from_slice(&self.new_consensus_public_key);
        bytes[53..117].copy_from_slice(&self.signature);
        bytes
    }

    pub fn decode_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != 117 {
            return None;
        }
        Some(Self::from_untrusted_parts(
            u32::from_be_bytes(bytes[0..4].try_into().ok()?),
            ValidatorRotationAuthority::from_tag(bytes[4])?,
            ValidatorId::new(u64::from_be_bytes(bytes[5..13].try_into().ok()?)),
            u64::from_be_bytes(bytes[13..21].try_into().ok()?),
            bytes[21..53].try_into().ok()?,
            bytes[53..117].try_into().ok()?,
        ))
    }

    pub const fn protocol_version(&self) -> u32 {
        self.protocol_version
    }

    pub const fn authority(&self) -> ValidatorRotationAuthority {
        self.authority
    }

    pub const fn validator_id(&self) -> ValidatorId {
        self.validator_id
    }

    pub const fn current_validator_set_version(&self) -> u64 {
        self.current_validator_set_version
    }

    pub const fn new_consensus_public_key(&self) -> [u8; 32] {
        self.new_consensus_public_key
    }

    pub const fn signature_bytes(&self) -> [u8; 64] {
        self.signature
    }

    pub fn verify(&self, current: &ValidatorCredential) -> Result<(), ValidatorRotationError> {
        if self.protocol_version != CURRENT_PROTOCOL_VERSION {
            return Err(ValidatorRotationError::UnsupportedProtocolVersion {
                expected: CURRENT_PROTOCOL_VERSION,
                actual: self.protocol_version,
            });
        }

        if self.validator_id != current.id() {
            return Err(ValidatorRotationError::ValidatorIdMismatch {
                expected: current.id(),
                actual: self.validator_id,
            });
        }

        VerifyingKey::from_bytes(&self.new_consensus_public_key)
            .map_err(|_| ValidatorRotationError::InvalidNewConsensusKey(self.validator_id))?;

        let authorizing_public_key = match self.authority {
            ValidatorRotationAuthority::Identity => current.identity_public_key(),
            ValidatorRotationAuthority::Recovery => current.recovery_public_key(),
        };

        let verifying_key = VerifyingKey::from_bytes(&authorizing_public_key)
            .map_err(|_| ValidatorRotationError::InvalidAuthorization(self.validator_id))?;
        let signature = Signature::from_bytes(&self.signature);
        let message = canonical_rotation_bytes(
            self.protocol_version,
            self.authority,
            self.validator_id,
            self.current_validator_set_version,
            self.new_consensus_public_key,
        );

        verifying_key
            .verify_strict(&message, &signature)
            .map_err(|_| ValidatorRotationError::InvalidAuthorization(self.validator_id))
    }
}

fn canonical_rotation_bytes(
    protocol_version: u32,
    authority: ValidatorRotationAuthority,
    validator_id: ValidatorId,
    current_validator_set_version: u64,
    new_consensus_public_key: [u8; 32],
) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(
        ROTATION_DOMAIN.len()
            + size_of::<u32>()
            + 1
            + size_of::<u64>() * 2
            + new_consensus_public_key.len(),
    );
    bytes.extend_from_slice(ROTATION_DOMAIN);
    bytes.extend_from_slice(&protocol_version.to_be_bytes());
    bytes.push(authority.tag());
    bytes.extend_from_slice(&validator_id.value().to_be_bytes());
    bytes.extend_from_slice(&current_validator_set_version.to_be_bytes());
    bytes.extend_from_slice(&new_consensus_public_key);
    bytes
}
