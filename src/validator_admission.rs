use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

use crate::{CURRENT_PROTOCOL_VERSION, ValidatorAdmissionError, ValidatorCredential, ValidatorId};

const ADMISSION_DOMAIN: &[u8] = b"SECOND_VALIDATOR_ADMISSION_V1\0";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatorAdmissionRequest {
    protocol_version: u32,
    credential: ValidatorCredential,
    identity_signature: [u8; 64],
    consensus_signature: [u8; 64],
    recovery_signature: [u8; 64],
}

impl ValidatorAdmissionRequest {
    pub fn sign(
        protocol_version: u32,
        credential: ValidatorCredential,
        identity_key: &SigningKey,
        consensus_key: &SigningKey,
        recovery_key: &SigningKey,
    ) -> Result<Self, ValidatorAdmissionError> {
        let message = canonical_admission_bytes(protocol_version, &credential);

        Ok(Self {
            protocol_version,
            credential,
            identity_signature: identity_key.sign(&message).to_bytes(),
            consensus_signature: consensus_key.sign(&message).to_bytes(),
            recovery_signature: recovery_key.sign(&message).to_bytes(),
        })
    }

    pub const fn protocol_version(&self) -> u32 {
        self.protocol_version
    }

    pub fn credential(&self) -> &ValidatorCredential {
        &self.credential
    }

    pub const fn identity_signature(&self) -> [u8; 64] {
        self.identity_signature
    }

    pub const fn consensus_signature(&self) -> [u8; 64] {
        self.consensus_signature
    }

    pub const fn recovery_signature(&self) -> [u8; 64] {
        self.recovery_signature
    }

    pub fn verify(&self) -> Result<VerifiedValidatorAdmission, ValidatorAdmissionError> {
        if self.protocol_version != CURRENT_PROTOCOL_VERSION {
            return Err(ValidatorAdmissionError::UnsupportedProtocolVersion {
                expected: CURRENT_PROTOCOL_VERSION,
                actual: self.protocol_version,
            });
        }

        let message = canonical_admission_bytes(self.protocol_version, &self.credential);
        let validator_id = self.credential.id();

        verify_proof(
            self.credential.identity_public_key(),
            self.identity_signature,
            &message,
            ValidatorAdmissionError::InvalidIdentityProof(validator_id),
        )?;
        verify_proof(
            self.credential.consensus_public_key(),
            self.consensus_signature,
            &message,
            ValidatorAdmissionError::InvalidConsensusProof(validator_id),
        )?;
        verify_proof(
            self.credential.recovery_public_key(),
            self.recovery_signature,
            &message,
            ValidatorAdmissionError::InvalidRecoveryProof(validator_id),
        )?;

        Ok(VerifiedValidatorAdmission {
            request: self.clone(),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedValidatorAdmission {
    request: ValidatorAdmissionRequest,
}

impl VerifiedValidatorAdmission {
    pub fn credential(&self) -> &ValidatorCredential {
        self.request.credential()
    }

    pub const fn validator_id(&self) -> ValidatorId {
        self.request.credential.id()
    }

    pub fn request(&self) -> &ValidatorAdmissionRequest {
        &self.request
    }
}

fn canonical_admission_bytes(protocol_version: u32, credential: &ValidatorCredential) -> Vec<u8> {
    let mut bytes =
        Vec::with_capacity(ADMISSION_DOMAIN.len() + size_of::<u32>() + size_of::<u64>() + 32 * 3);
    bytes.extend_from_slice(ADMISSION_DOMAIN);
    bytes.extend_from_slice(&protocol_version.to_be_bytes());
    bytes.extend_from_slice(&credential.id().value().to_be_bytes());
    bytes.extend_from_slice(&credential.identity_public_key());
    bytes.extend_from_slice(&credential.consensus_public_key());
    bytes.extend_from_slice(&credential.recovery_public_key());
    bytes
}

fn verify_proof(
    public_key: [u8; 32],
    signature: [u8; 64],
    message: &[u8],
    error: ValidatorAdmissionError,
) -> Result<(), ValidatorAdmissionError> {
    let verifying_key = VerifyingKey::from_bytes(&public_key).map_err(|_| error.clone())?;
    let signature = Signature::from_bytes(&signature);

    verifying_key
        .verify_strict(message, &signature)
        .map_err(|_| error)
}
