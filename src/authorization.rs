use std::collections::BTreeSet;

use crate::{
    AccountAddress, AuthorizationError, LegalTaskPayload, Operation, SignatureParseError,
    TaskEncodingError, TaskId,
};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};

const REQUEST_DIGEST_DOMAIN: &[u8] = b"SECOND_SIGNED_LEGAL_TASK_V1\0";
const ACCOUNT_SIGNATURE_DOMAIN: &[u8] = b"Second/AccountAuthorization/v1\0";
const NETWORK_SIGNATURE_DOMAIN: &[u8] = b"Second/LegalTaskNetwork/v1\0";
pub const MAX_ACCOUNT_SIGNATURES: usize = 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccountSignature {
    pub account: AccountAddress,
    pub signature: [u8; 64],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegalTask {
    payload: LegalTaskPayload,
    network_id: [u8; 32],
    authorizer_public_key: [u8; 32],
    signature: [u8; 64],
    account_signatures: Vec<AccountSignature>,
}

impl LegalTask {
    pub fn sign(
        payload: LegalTaskPayload,
        signing_key: &SigningKey,
    ) -> Result<Self, TaskEncodingError> {
        Self::sign_in_network(payload, signing_key, [0; 32])
    }

    pub fn sign_in_network(
        payload: LegalTaskPayload,
        key: &SigningKey,
        network_id: [u8; 32],
    ) -> Result<Self, TaskEncodingError> {
        let mut task = Self::from_signed_parts(
            payload,
            network_id,
            key.verifying_key().to_bytes(),
            [0; 64],
            Vec::new(),
        );
        task.signature = key.sign(&task.canonical_signing_bytes()?).to_bytes();
        Ok(task)
    }

    pub fn sign_account(
        payload: LegalTaskPayload,
        key: &SigningKey,
    ) -> Result<Self, TaskEncodingError> {
        Self::sign_account_in_network(payload, key, [0; 32])
    }

    pub fn sign_account_in_network(
        payload: LegalTaskPayload,
        key: &SigningKey,
        network_id: [u8; 32],
    ) -> Result<Self, TaskEncodingError> {
        let mut task = Self::from_signed_parts(payload, network_id, [0; 32], [0; 64], Vec::new());
        task.add_account_signature(key)?;
        Ok(task)
    }

    pub fn add_account_signature(&mut self, key: &SigningKey) -> Result<(), TaskEncodingError> {
        let account = AccountAddress::from_bytes(key.verifying_key().to_bytes());
        let signature = key.sign(&self.account_signing_bytes()?).to_bytes();
        self.with_account_signature(AccountSignature { account, signature });
        Ok(())
    }

    pub fn with_account_signature_base64url(
        &mut self,
        account: AccountAddress,
        signature: &str,
    ) -> Result<(), SignatureParseError> {
        let signature = parse_signature_base64url(signature)?;
        self.with_account_signature(AccountSignature { account, signature });
        Ok(())
    }

    pub fn with_account_signature(&mut self, entry: AccountSignature) {
        match self
            .account_signatures
            .binary_search_by_key(&entry.account, |item| item.account)
        {
            Ok(index) => self.account_signatures[index] = entry,
            Err(index) => self.account_signatures.insert(index, entry),
        }
    }

    pub fn from_signed_parts(
        payload: LegalTaskPayload,
        network_id: [u8; 32],
        authorizer_public_key: [u8; 32],
        signature: [u8; 64],
        account_signatures: Vec<AccountSignature>,
    ) -> Self {
        Self {
            payload,
            network_id,
            authorizer_public_key,
            signature,
            account_signatures,
        }
    }

    pub fn from_parts(
        payload: LegalTaskPayload,
        authorizer_public_key: [u8; 32],
        signature: [u8; 64],
    ) -> Self {
        Self {
            payload,
            network_id: [0; 32],
            authorizer_public_key,
            signature,
            account_signatures: Vec::new(),
        }
    }

    pub fn from_signature_base64url(
        payload: LegalTaskPayload,
        authorizer_public_key: [u8; 32],
        signature: &str,
    ) -> Result<Self, SignatureParseError> {
        let signature = parse_signature_base64url(signature)?;
        Ok(Self::from_parts(payload, authorizer_public_key, signature))
    }

    pub const fn network_id(&self) -> [u8; 32] {
        self.network_id
    }

    pub fn from_signature_base64url_in_network(
        payload: LegalTaskPayload,
        network_id: [u8; 32],
        public_key: [u8; 32],
        signature: &str,
    ) -> Result<Self, SignatureParseError> {
        let signature = parse_signature_base64url(signature)?;
        Ok(Self::from_signed_parts(
            payload,
            network_id,
            public_key,
            signature,
            Vec::new(),
        ))
    }

    pub fn signature_base64url(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.signature)
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

    pub fn account_signatures(&self) -> &[AccountSignature] {
        &self.account_signatures
    }

    pub fn has_account_signature(&self, account: AccountAddress) -> bool {
        self.account_signatures
            .binary_search_by_key(&account, |entry| entry.account)
            .is_ok()
    }

    fn account_signing_bytes(&self) -> Result<Vec<u8>, TaskEncodingError> {
        let mut message = ACCOUNT_SIGNATURE_DOMAIN.to_vec();
        message.extend_from_slice(&self.canonical_signing_bytes()?);
        Ok(message)
    }

    pub fn canonical_signing_bytes(&self) -> Result<Vec<u8>, TaskEncodingError> {
        let mut bytes = NETWORK_SIGNATURE_DOMAIN.to_vec();
        bytes.extend_from_slice(&self.network_id);
        bytes.extend_from_slice(&self.payload.canonical_signing_bytes()?);
        Ok(bytes)
    }

    pub fn verify(
        &self,
        authorizers: &AuthorizerSet,
    ) -> Result<VerifiedLegalTask, AuthorizationError> {
        self.payload
            .validate()
            .map_err(AuthorizationError::InvalidPayload)?;

        if self.payload.protocol_version() != authorizers.protocol_version() {
            return Err(AuthorizationError::UnsupportedProtocolVersion {
                expected: authorizers.protocol_version(),
                actual: self.payload.protocol_version(),
            });
        }

        if self.network_id != authorizers.network_id() {
            return Err(AuthorizationError::WrongNetwork);
        }
        let message = self
            .canonical_signing_bytes()
            .map_err(AuthorizationError::Encoding)?;
        let needs_authorizer = self.payload.operations().iter().any(|operation| {
            matches!(
                operation,
                Operation::Issue { .. } | Operation::Destroy { .. } | Operation::LeakRepair { .. }
            )
        });
        if self.authorizer_public_key == [0; 32] {
            if needs_authorizer || self.signature != [0; 64] {
                return Err(AuthorizationError::UntrustedAuthorizer);
            }
        } else {
            if !authorizers.contains(self.authorizer_public_key) {
                return Err(AuthorizationError::UntrustedAuthorizer);
            }
            VerifyingKey::from_bytes(&self.authorizer_public_key)
                .map_err(|_| AuthorizationError::InvalidPublicKey)?
                .verify_strict(&message, &Signature::from_bytes(&self.signature))
                .map_err(|_| AuthorizationError::InvalidSignature)?;
        }
        self.verify_account_signatures()?;
        crate::prepared::source::validate_allocation_source(self)?;

        let request_digest = self.request_digest_from_message(&message);

        Ok(VerifiedLegalTask {
            task: self.clone(),
            request_digest,
        })
    }

    pub fn verify_account_signatures(&self) -> Result<(), AuthorizationError> {
        self.payload
            .validate()
            .map_err(AuthorizationError::InvalidPayload)?;
        if self.account_signatures.len() > MAX_ACCOUNT_SIGNATURES {
            return Err(AuthorizationError::TooManyAccountSignatures);
        }
        let account_message = self
            .account_signing_bytes()
            .map_err(AuthorizationError::Encoding)?;
        let mut previous = None;
        for entry in &self.account_signatures {
            if previous.is_some_and(|last| last >= entry.account) {
                return Err(AuthorizationError::NonCanonicalAccountSignatures);
            }
            VerifyingKey::from_bytes(&entry.account.bytes())
                .map_err(|_| AuthorizationError::InvalidPublicKey)?
                .verify_strict(&account_message, &Signature::from_bytes(&entry.signature))
                .map_err(|_| AuthorizationError::InvalidSignature)?;
            previous = Some(entry.account);
        }

        Ok(())
    }

    pub(crate) fn request_digest(&self) -> Result<[u8; 32], TaskEncodingError> {
        let message = self.canonical_signing_bytes()?;
        Ok(self.request_digest_from_message(&message))
    }

    fn request_digest_from_message(&self, message: &[u8]) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(REQUEST_DIGEST_DOMAIN);
        hasher.update(self.authorizer_public_key);
        hasher.update(self.signature);
        hasher.update((self.account_signatures.len() as u32).to_be_bytes());
        for entry in &self.account_signatures {
            hasher.update(entry.account.bytes());
            hasher.update(entry.signature);
        }
        hasher.update(message);
        hasher.finalize().into()
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

    pub fn task_id(&self) -> TaskId {
        self.task.payload.task_id()
    }

    pub const fn expires_at(&self) -> Option<u64> {
        self.task.payload.expires_at()
    }

    pub fn require_account_signature(
        &self,
        account: AccountAddress,
    ) -> Result<(), crate::ExecutionError> {
        if self.task.has_account_signature(account) {
            Ok(())
        } else {
            Err(crate::ExecutionError::MissingAccountSignature(account))
        }
    }

    pub fn operations(&self) -> &[Operation] {
        self.task.payload.operations()
    }

    pub fn is_expired(&self, now: u64) -> bool {
        self.expires_at().is_some_and(|expires_at| now > expires_at)
    }

    pub fn signed_task(&self) -> &LegalTask {
        &self.task
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizerSet {
    protocol_version: u32,
    network_id: [u8; 32],
    public_keys: BTreeSet<[u8; 32]>,
}

impl AuthorizerSet {
    pub fn new<I>(protocol_version: u32, public_keys: I) -> Result<Self, AuthorizationError>
    where
        I: IntoIterator<Item = [u8; 32]>,
    {
        Self::new_for_network(protocol_version, [0; 32], public_keys)
    }

    pub fn new_for_network<I>(
        protocol_version: u32,
        network_id: [u8; 32],
        public_keys: I,
    ) -> Result<Self, AuthorizationError>
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
            network_id,
            public_keys: set,
        })
    }

    pub const fn network_id(&self) -> [u8; 32] {
        self.network_id
    }

    pub const fn protocol_version(&self) -> u32 {
        self.protocol_version
    }

    pub fn contains(&self, public_key: [u8; 32]) -> bool {
        self.public_keys.contains(&public_key)
    }
}

fn parse_signature_base64url(value: &str) -> Result<[u8; 64], SignatureParseError> {
    if value.len() != 86 {
        return Err(SignatureParseError::WrongEncodedLength);
    }

    let decoded = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| SignatureParseError::InvalidBase64Url)?;
    let signature: [u8; 64] = decoded
        .try_into()
        .map_err(|_| SignatureParseError::WrongDecodedLength)?;

    if URL_SAFE_NO_PAD.encode(signature) != value {
        return Err(SignatureParseError::NonCanonical);
    }

    Ok(signature)
}
