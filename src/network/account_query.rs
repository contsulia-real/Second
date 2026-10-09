//! Private account data: possession of the account key, bound to this QUIC session.
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};

use super::{NetworkError, NetworkMessage};

mod client;
mod server;
pub use client::{client_account_query, client_account_view};
pub(super) use server::serve;

#[cfg(test)]
mod tests;

pub const MAX_ACCOUNT_QUERY_PAGE: u16 = 128;
pub const MAX_ACCOUNT_QUERY_ROWS: usize = 100_000;
pub const ACCOUNT_QUERY_LIFETIME: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountPaymentAddress {
    pub address: String,
    pub status: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountTransfer {
    pub task_id: String,
    pub operation_index: u64,
    pub source: String,
    pub destination: String,
    pub amount: u64,
    pub incoming: bool,
    pub outgoing: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountView {
    pub account: String,
    pub kind: u8,
    pub cursor: u64,
    pub generation: u64,
    pub validator_set_version: u64,
    pub exists: bool,
    pub balance: u64,
    pub total: u64,
    pub addresses: Vec<AccountPaymentAddress>,
    pub transfers: Vec<AccountTransfer>,
    pub currencies: Vec<String>,
    pub next: Option<u64>,
    pub nonce: Vec<u8>,
}

fn signing_bytes(binding: [u8; 32], message: &NetworkMessage) -> Result<Vec<u8>, NetworkError> {
    let NetworkMessage::AccountQuery {
        account,
        kind,
        cursor,
        generation,
        nonce,
        ..
    } = message
    else {
        return Err(NetworkError::UnexpectedMessage);
    };
    validate_request(*kind, *cursor, *generation)?;
    let mut bytes = b"Second/AccountQuery/v1\0".to_vec();
    bytes.extend_from_slice(&binding);
    bytes.extend_from_slice(account);
    bytes.push(*kind);
    bytes.extend_from_slice(&cursor.to_be_bytes());
    bytes.push(u8::from(generation.is_some()));
    bytes.extend_from_slice(&generation.unwrap_or(0).to_be_bytes());
    bytes.extend_from_slice(nonce);
    Ok(bytes)
}

pub(super) fn validate_request(
    kind: u8,
    cursor: u64,
    generation: Option<u64>,
) -> Result<(), NetworkError> {
    if !(1..=4).contains(&kind)
        || cursor >= MAX_ACCOUNT_QUERY_ROWS as u64
        || !cursor.is_multiple_of(u64::from(MAX_ACCOUNT_QUERY_PAGE))
        || (cursor == 0) != generation.is_none()
        || generation == Some(0)
        || (kind == 1 && cursor != 0)
    {
        return Err(NetworkError::InvalidAccountQuery);
    }
    Ok(())
}

pub(super) fn authenticate(
    binding: [u8; 32],
    message: &NetworkMessage,
) -> Result<(), NetworkError> {
    let NetworkMessage::AccountQuery {
        account, signature, ..
    } = message
    else {
        return Err(NetworkError::UnexpectedMessage);
    };
    let bytes = signing_bytes(binding, message)?;
    let key = VerifyingKey::from_bytes(account).map_err(|_| NetworkError::InvalidAccountQuery)?;
    key.verify_strict(&bytes, &Signature::from_bytes(signature))
        .map_err(|_| NetworkError::InvalidAccountQuery)
}

pub(super) fn rejected(reason: &str) -> NetworkMessage {
    NetworkMessage::AccountQueryDenied {
        reason: reason.to_owned(),
    }
}
