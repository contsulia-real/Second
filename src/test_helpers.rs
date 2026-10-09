//! Deterministic cryptographic identities for protocol tests.
//! Test transactions must exercise the same account-key verification as production.
use std::collections::BTreeSet;
use std::sync::OnceLock;

use ed25519_dalek::SigningKey;

use crate::{
    AccountAddress, LegalTask, LegalTaskPayload, Operation, PaymentAddress, TaskEncodingError,
};

pub(crate) fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

pub(crate) fn key_u16(seed: u16) -> SigningKey {
    let mut bytes = [0; 32];
    bytes[..2].copy_from_slice(&seed.to_be_bytes());
    SigningKey::from_bytes(&bytes)
}

pub(crate) fn account_u16(seed: u16) -> AccountAddress {
    AccountAddress::from_bytes(key_u16(seed).verifying_key().to_bytes())
}

pub(crate) fn account(seed: u8) -> AccountAddress {
    AccountAddress::from_bytes(key(seed).verifying_key().to_bytes())
}

fn account_seeds() -> &'static [AccountAddress; 256] {
    static ADDRESSES: OnceLock<[AccountAddress; 256]> = OnceLock::new();
    ADDRESSES.get_or_init(|| std::array::from_fn(|seed| account(seed as u8)))
}

fn payment_owner_seed(address: PaymentAddress) -> Option<u8> {
    let bytes = address.bytes();
    if bytes.iter().all(|byte| *byte == bytes[0]) {
        return Some(bytes[0]);
    }
    account_seeds()
        .iter()
        .position(|account| account.bytes() == bytes)
        .and_then(|index| u8::try_from(index).ok())
}

pub(crate) fn sign(
    payload: LegalTaskPayload,
    authorizer: &SigningKey,
) -> Result<LegalTask, TaskEncodingError> {
    let mut owners = BTreeSet::new();
    for op in payload.operations() {
        match op {
            Operation::RegisterAccount { account }
            | Operation::RegisterPaymentAddress { account, .. } => {
                owners.insert(*account);
            }
            Operation::Transfer { source, .. } => {
                if let Some(seed) = payment_owner_seed(*source) {
                    owners.insert(account(seed));
                }
            }
            Operation::RetirePaymentAddress { address }
            | Operation::FinalizePaymentAddressRetirement { address } => {
                if let Some(seed) = payment_owner_seed(*address) {
                    owners.insert(account(seed));
                }
            }
            Operation::Issue { .. } | Operation::Destroy { .. } | Operation::LeakRepair { .. } => {}
        }
    }
    let mut task = LegalTask::sign(payload, authorizer)?;
    for (seed, owner) in account_seeds().iter().enumerate() {
        if owners.contains(owner) {
            task.add_account_signature(&key(seed as u8))?;
        }
    }
    Ok(task)
}
