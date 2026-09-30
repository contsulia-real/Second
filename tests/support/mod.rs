#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use ed25519_dalek::{Signer, SigningKey};
use second::{
    AccountAddress, AuthorizerSet, CURRENT_PROTOCOL_VERSION, FinalityCertificate,
    FinalityStatement, LegalTask, LegalTaskPayload, Operation, PaymentAddress, SecondState, TaskId,
    ValidatorCredential, ValidatorId, ValidatorSet, ValidatorVote, VerifiedLegalTask,
};

pub fn key(byte: u8) -> SigningKey {
    SigningKey::from_bytes(&[byte; 32])
}

fn deterministic_address_bytes(value: u64) -> [u8; 32] {
    let mut bytes = [0_u8; 32];
    bytes[24..].copy_from_slice(&value.to_be_bytes());
    bytes
}

pub fn account(value: u64) -> AccountAddress {
    AccountAddress::from_bytes(deterministic_address_bytes(value))
}

pub fn payment(value: u64) -> PaymentAddress {
    PaymentAddress::from_bytes(deterministic_address_bytes(value))
}

pub fn task_id(value: u128) -> TaskId {
    TaskId::parse(&format!("t{value:032x}")).unwrap()
}

pub fn verified_task(task_number: u128, operations: Vec<Operation>) -> VerifiedLegalTask {
    verified_task_with_expiry(task_number, None, operations)
}

pub fn verified_task_with_expiry(
    task_number: u128,
    expires_at: Option<u64>,
    operations: Vec<Operation>,
) -> VerifiedLegalTask {
    let signing = key(9);
    let authorizers = AuthorizerSet::new(
        CURRENT_PROTOCOL_VERSION,
        [signing.verifying_key().to_bytes()],
    )
    .unwrap();
    let payload = LegalTaskPayload::new(
        task_id(task_number),
        CURRENT_PROTOCOL_VERSION,
        expires_at.unwrap_or(u64::MAX),
        operations,
    );

    LegalTask::sign(payload, &signing)
        .unwrap()
        .verify(&authorizers)
        .unwrap()
}

pub fn payment_address(account: AccountAddress) -> PaymentAddress {
    PaymentAddress::from_bytes(account.bytes())
}

pub fn register_payment_addresses(
    state: &mut SecondState,
    accounts: impl IntoIterator<Item = AccountAddress>,
) {
    for account in accounts {
        state
            .register_payment_address(payment_address(account), account)
            .unwrap();
    }
}

pub fn validator_credential(id: u64) -> ValidatorCredential {
    ValidatorCredential::new(
        ValidatorId::new(id),
        key((id * 3) as u8).verifying_key().to_bytes(),
        key((id * 3 + 1) as u8).verifying_key().to_bytes(),
        key((id * 3 + 2) as u8).verifying_key().to_bytes(),
    )
    .unwrap()
}

pub fn validator_set(version: u64, ids: impl IntoIterator<Item = u64>) -> ValidatorSet {
    ValidatorSet::new(version, ids.into_iter().map(validator_credential)).unwrap()
}

pub fn signed_vote(
    statement: &FinalityStatement,
    validator_id: ValidatorId,
    signing_key: &SigningKey,
) -> ValidatorVote {
    let signature = signing_key
        .sign(&statement.canonical_signing_bytes())
        .to_bytes();
    ValidatorVote::from_untrusted_parts(validator_id, signature)
}

pub fn certificate_from_keys(
    statement: FinalityStatement,
    validator_set: &ValidatorSet,
    signers: impl IntoIterator<Item = (ValidatorId, SigningKey)>,
) -> FinalityCertificate {
    let votes = signers
        .into_iter()
        .map(|(validator_id, signing_key)| signed_vote(&statement, validator_id, &signing_key))
        .collect();
    FinalityCertificate::new(statement, votes, validator_set).unwrap()
}

pub fn temp_base(prefix: &str) -> PathBuf {
    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    let unique = format!(
        "second-{prefix}-{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must be after unix epoch")
            .as_nanos(),
        NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed),
    );
    std::env::temp_dir().join(unique)
}
