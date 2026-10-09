//! Private account data: possession of the account key, bound to this QUIC session.
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

use super::{NetworkError, NetworkMessage, QuicPeer};
use crate::{AccountAddress, PaymentAddressStatus, PersistedNodeState};

pub const MAX_ACCOUNT_QUERY_PAGE: u16 = 128;

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
    if !(1..=4).contains(kind) {
        return Err(NetworkError::UnexpectedMessage);
    }
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

pub async fn client_account_query(
    peer: &QuicPeer,
    key: &SigningKey,
    kind: u8,
    cursor: u64,
    generation: Option<u64>,
) -> Result<AccountView, NetworkError> {
    let mut nonce = [0; 32];
    getrandom::fill(&mut nonce).map_err(|error| NetworkError::Transport(error.to_string()))?;
    let account = key.verifying_key().to_bytes();
    let mut message = NetworkMessage::AccountQuery {
        account,
        kind,
        cursor,
        generation,
        nonce,
        signature: [0; 64],
    };
    let signature_bytes = key
        .sign(&signing_bytes(peer.channel_binding()?, &message)?)
        .to_bytes();
    if let NetworkMessage::AccountQuery { signature, .. } = &mut message {
        *signature = signature_bytes;
    }
    match peer.exchange(&message).await? {
        NetworkMessage::AccountQueryResult { view } => {
            if view.account != AccountAddress::from_bytes(account).to_string()
                || view.kind != kind
                || view.cursor != cursor
                || view.nonce != nonce
                || generation.is_some_and(|expected| expected != view.generation)
                || view.addresses.len() > usize::from(MAX_ACCOUNT_QUERY_PAGE)
                || view.transfers.len() > usize::from(MAX_ACCOUNT_QUERY_PAGE)
                || view.currencies.len() > usize::from(MAX_ACCOUNT_QUERY_PAGE)
                || view.next.is_some_and(|next| {
                    next != cursor.saturating_add(u64::from(MAX_ACCOUNT_QUERY_PAGE))
                })
                || (!view.exists
                    && (view.balance != 0
                        || !view.addresses.is_empty()
                        || !view.transfers.is_empty()
                        || !view.currencies.is_empty()))
            {
                return Err(NetworkError::InvalidAccountQuery);
            }
            for row in &view.addresses {
                crate::PaymentAddress::parse(&row.address)
                    .map_err(|_| NetworkError::InvalidAccountQuery)?;
                if !matches!(row.status.as_str(), "active" | "retiring" | "retired") {
                    return Err(NetworkError::InvalidAccountQuery);
                }
            }
            for row in &view.transfers {
                crate::TaskId::parse(&row.task_id)
                    .map_err(|_| NetworkError::InvalidAccountQuery)?;
                crate::PaymentAddress::parse(&row.source)
                    .map_err(|_| NetworkError::InvalidAccountQuery)?;
                crate::PaymentAddress::parse(&row.destination)
                    .map_err(|_| NetworkError::InvalidAccountQuery)?;
                if row.amount == 0 || (!row.incoming && !row.outgoing) {
                    return Err(NetworkError::InvalidAccountQuery);
                }
            }
            for address in &view.currencies {
                crate::CurrencyAddress::parse(address)
                    .map_err(|_| NetworkError::InvalidAccountQuery)?;
            }
            Ok(view)
        }
        NetworkMessage::AccountQueryDenied { reason } => {
            Err(NetworkError::AccountQueryDenied(reason))
        }
        _ => Err(NetworkError::UnexpectedMessage),
    }
}

pub(crate) fn response(
    peer: &QuicPeer,
    message: &NetworkMessage,
    snapshot: Option<&PersistedNodeState>,
) -> Result<NetworkMessage, NetworkError> {
    let NetworkMessage::AccountQuery {
        account,
        kind,
        cursor,
        generation,
        nonce,
        signature,
    } = message
    else {
        return Err(NetworkError::UnexpectedMessage);
    };
    let rejected = |reason: &str| NetworkMessage::AccountQueryDenied {
        reason: reason.to_owned(),
    };
    let Ok(key) = VerifyingKey::from_bytes(account) else {
        return Ok(rejected("unauthorized"));
    };
    let bytes = signing_bytes(peer.channel_binding()?, message)?;
    if key
        .verify_strict(&bytes, &Signature::from_bytes(signature))
        .is_err()
    {
        return Ok(rejected("unauthorized"));
    }
    let Some(snapshot) = snapshot else {
        return Ok(rejected("unavailable"));
    };
    if generation.is_some_and(|expected| expected != snapshot.generation) {
        return Ok(rejected("state_changed"));
    }
    let address = AccountAddress::from_bytes(*account);
    let state = &snapshot.state;
    let mut view = AccountView {
        account: address.to_string(),
        kind: *kind,
        cursor: *cursor,
        generation: snapshot.generation,
        validator_set_version: snapshot.validator_set.version(),
        exists: state.has_account(address),
        balance: state.balance(address),
        addresses: Vec::new(),
        transfers: Vec::new(),
        currencies: Vec::new(),
        next: None,
        nonce: nonce.to_vec(),
    };
    let skip = usize::try_from(*cursor).map_err(|_| NetworkError::InvalidAccountQuery)?;
    let limit = usize::from(MAX_ACCOUNT_QUERY_PAGE);
    match kind {
        1 if *cursor == 0 => {}
        2 => {
            let mut rows = state
                .business
                .payment_addresses
                .iter()
                .filter(|(_, record)| record.account == address)
                .skip(skip);
            view.addresses = rows
                .by_ref()
                .take(limit)
                .map(|(payment, record)| AccountPaymentAddress {
                    address: payment.to_string(),
                    status: match record.status {
                        PaymentAddressStatus::Active => "active",
                        PaymentAddressStatus::Retiring => "retiring",
                        PaymentAddressStatus::Retired => "retired",
                    }
                    .to_owned(),
                })
                .collect();
            if rows.next().is_some() {
                view.next = cursor.checked_add(limit as u64);
            }
        }
        3 => {
            let mut rows = state
                .business
                .payment_history
                .iter()
                .filter(|(_, execution)| {
                    state.payment_address_account(execution.source) == Some(address)
                        || state.payment_address_account(execution.destination) == Some(address)
                })
                .skip(skip);
            view.transfers = rows
                .by_ref()
                .take(limit)
                .map(|(id, execution)| AccountTransfer {
                    task_id: id.task_id().to_string(),
                    operation_index: id.operation_index(),
                    source: execution.source.to_string(),
                    destination: execution.destination.to_string(),
                    amount: execution.amount,
                    incoming: state.payment_address_account(execution.destination) == Some(address),
                    outgoing: state.payment_address_account(execution.source) == Some(address),
                })
                .collect();
            if rows.next().is_some() {
                view.next = cursor.checked_add(limit as u64);
            }
        }
        4 => {
            let mut rows = state
                .business
                .currencies
                .iter()
                .filter(|(_, currency)| currency.owner == Some(address))
                .skip(skip);
            view.currencies = rows
                .by_ref()
                .take(limit)
                .map(|(currency, _)| currency.to_string())
                .collect();
            if rows.next().is_some() {
                view.next = cursor.checked_add(limit as u64);
            }
        }
        _ => return Err(NetworkError::InvalidAccountQuery),
    }
    Ok(NetworkMessage::AccountQueryResult { view })
}
