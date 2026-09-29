use std::collections::BTreeSet;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};

use crate::{AuthorizationError, LegalTaskPayload, Operation, TaskEncodingError, TaskId};

const REQUEST_DIGEST_DOMAIN: &[u8] = b"SECOND_SIGNED_LEGAL_TASK_V1\0";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegalTask {
    payload: LegalTaskPayload,
    authorizer_public_key: [u8; 32],
    signature: [u8; 64],
}

impl LegalTask {
    pub fn sign(
        payload: LegalTaskPayload,
        signing_key: &SigningKey,
    ) -> Result<Self, TaskEncodingError> {
        let message = payload.canonical_signing_bytes()?;
        let signature = signing_key.sign(&message).to_bytes();

        Ok(Self {
            payload,
            authorizer_public_key: signing_key.verifying_key().to_bytes(),
            signature,
        })
    }

    pub fn from_parts(
        payload: LegalTaskPayload,
        authorizer_public_key: [u8; 32],
        signature: [u8; 64],
    ) -> Self {
        Self {
            payload,
            authorizer_public_key,
            signature,
        }
    }

    pub fn payload(&self) -> &LegalTaskPayload {
        &self.payload
    }

    pub const fn authorizer_public_key(&self) -> [u8; 32] {
        self.authorizer_public_key
    }

    pub const fn signature_bytes(&self) -> [u8; 64] {
        self.signature
    }

    pub fn canonical_signing_bytes(&self) -> Result<Vec<u8>, TaskEncodingError> {
        self.payload.canonical_signing_bytes()
    }

    pub fn verify(
        &self,
        authorizers: &AuthorizerSet,
    ) -> Result<VerifiedLegalTask, AuthorizationError> {
        if self.payload.protocol_version() != authorizers.protocol_version() {
            return Err(AuthorizationError::UnsupportedProtocolVersion {
                expected: authorizers.protocol_version(),
                actual: self.payload.protocol_version(),
            });
        }

        if !authorizers.contains(self.authorizer_public_key) {
            return Err(AuthorizationError::UntrustedAuthorizer);
        }

        let message = self
            .payload
            .canonical_signing_bytes()
            .map_err(AuthorizationError::Encoding)?;

        let verifying_key = VerifyingKey::from_bytes(&self.authorizer_public_key)
            .map_err(|_| AuthorizationError::InvalidPublicKey)?;
        let signature = Signature::from_bytes(&self.signature);

        verifying_key
            .verify_strict(&message, &signature)
            .map_err(|_| AuthorizationError::InvalidSignature)?;

        let request_digest = request_digest(&message, &self.authorizer_public_key, &self.signature);

        Ok(VerifiedLegalTask {
            task: self.clone(),
            request_digest,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedLegalTask {
    task: LegalTask,
    request_digest: [u8; 32],
}

impl VerifiedLegalTask {
    pub const fn request_digest(&self) -> [u8; 32] {
        self.request_digest
    }

    pub fn payload(&self) -> &LegalTaskPayload {
        self.task.payload()
    }

    pub const fn task_id(&self) -> TaskId {
        self.task.payload.task_id()
    }

    pub const fn expires_at(&self) -> Option<u64> {
        self.task.payload.expires_at()
    }

    pub fn operations(&self) -> &[Operation] {
        self.task.payload.operations()
    }

    pub fn signed_task(&self) -> &LegalTask {
        &self.task
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizerSet {
    protocol_version: u32,
    public_keys: BTreeSet<[u8; 32]>,
}

impl AuthorizerSet {
    pub fn new<I>(protocol_version: u32, public_keys: I) -> Result<Self, AuthorizationError>
    where
        I: IntoIterator<Item = [u8; 32]>,
    {
        let mut set = BTreeSet::new();

        for public_key in public_keys {
            if !set.insert(public_key) {
                return Err(AuthorizationError::DuplicateAuthorizer);
            }
        }

        if set.is_empty() {
            return Err(AuthorizationError::EmptyAuthorizerSet);
        }

        Ok(Self {
            protocol_version,
            public_keys: set,
        })
    }

    pub const fn protocol_version(&self) -> u32 {
        self.protocol_version
    }

    pub fn contains(&self, public_key: [u8; 32]) -> bool {
        self.public_keys.contains(&public_key)
    }
}

fn request_digest(message: &[u8], public_key: &[u8; 32], signature: &[u8; 64]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(REQUEST_DIGEST_DOMAIN);
    hasher.update(public_key);
    hasher.update(signature);
    hasher.update(message);
    hasher.finalize().into()
}
