//! Validate pages and complete enumerations through the same library client.
use ed25519_dalek::{Signer, SigningKey};

use super::*;
use crate::network::QuicPeer;
use crate::{AccountAddress, CurrencyAddress, PaymentAddress, TaskId};

pub async fn client_account_query(
    peer: &QuicPeer,
    key: &SigningKey,
    kind: u8,
    cursor: u64,
    generation: Option<u64>,
) -> Result<AccountView, NetworkError> {
    validate_request(kind, cursor, generation)?;
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
    let signed = key
        .sign(&signing_bytes(peer.channel_binding()?, &message)?)
        .to_bytes();
    if let NetworkMessage::AccountQuery { signature, .. } = &mut message {
        *signature = signed;
    }
    match peer.exchange(&message).await? {
        NetworkMessage::AccountQueryResult { view } => {
            if view.account != AccountAddress::from_bytes(account).to_string()
                || view.kind != kind
                || view.cursor != cursor
                || view.nonce != nonce
                || generation.is_some_and(|expected| expected != view.generation)
            {
                return Err(NetworkError::InvalidAccountQuery);
            }
            validate_page(&view)?;
            Ok(view)
        }
        NetworkMessage::AccountQueryDenied { reason } => {
            Err(NetworkError::AccountQueryDenied(reason))
        }
        _ => Err(NetworkError::UnexpectedMessage),
    }
}

pub async fn client_account_view(
    peer: &QuicPeer,
    key: &SigningKey,
    kind: u8,
) -> Result<AccountView, NetworkError> {
    tokio::time::timeout(ACCOUNT_QUERY_LIFETIME, async {
        let mut combined: Option<AccountView> = None;
        let mut cursor = 0;
        loop {
            let generation = combined.as_ref().map(|view| view.generation);
            let page = client_account_query(peer, key, kind, cursor, generation).await?;
            let next = page.next;
            if let Some(view) = &mut combined {
                append_page(view, page)?;
            } else {
                combined = Some(page);
            }
            match next {
                Some(next) => cursor = next,
                None => {
                    let view = combined.unwrap();
                    let rows = view.addresses.len() + view.transfers.len() + view.currencies.len();
                    if rows as u64 != view.total || (kind == 4 && rows as u64 != view.balance) {
                        return Err(NetworkError::InvalidAccountQuery);
                    }
                    return Ok(view);
                }
            }
        }
    })
    .await
    .map_err(|_| NetworkError::Transport("account query timed out".to_owned()))?
}

pub(super) fn validate_page(view: &AccountView) -> Result<(), NetworkError> {
    let invalid = || NetworkError::InvalidAccountQuery;
    AccountAddress::parse(&view.account).map_err(|_| invalid())?;
    let rows = match view.kind {
        1 if view.addresses.is_empty()
            && view.transfers.is_empty()
            && view.currencies.is_empty() =>
        {
            0
        }
        2 if view.transfers.is_empty() && view.currencies.is_empty() => view.addresses.len(),
        3 if view.addresses.is_empty() && view.currencies.is_empty() => view.transfers.len(),
        4 if view.addresses.is_empty() && view.transfers.is_empty() => view.currencies.len(),
        _ => return Err(invalid()),
    };
    let end = view.cursor.checked_add(rows as u64).ok_or_else(invalid)?;
    if view.generation == 0
        || view.validator_set_version == 0
        || view.nonce.len() != 32
        || view.total > MAX_ACCOUNT_QUERY_ROWS as u64
        || rows > usize::from(MAX_ACCOUNT_QUERY_PAGE)
        || !view
            .cursor
            .is_multiple_of(u64::from(MAX_ACCOUNT_QUERY_PAGE))
        || end > view.total
        || (rows == 0 && (view.cursor != 0 || view.total != 0))
        || (view.kind == 1 && (view.cursor != 0 || view.total != 0))
        || (view.kind == 4 && view.total != view.balance)
        || (!view.exists && (view.balance != 0 || view.total != 0 || view.next.is_some()))
        || if end < view.total {
            rows != usize::from(MAX_ACCOUNT_QUERY_PAGE) || view.next != Some(end)
        } else {
            view.next.is_some()
        }
    {
        return Err(invalid());
    }
    let mut last_payment = None;
    for row in &view.addresses {
        let address = PaymentAddress::parse(&row.address).map_err(|_| invalid())?;
        if last_payment.is_some_and(|last| address <= last)
            || !matches!(row.status.as_str(), "active" | "retiring" | "retired")
        {
            return Err(invalid());
        }
        last_payment = Some(address);
    }
    let mut last_transfer = None;
    for row in &view.transfers {
        let task = TaskId::parse(&row.task_id).map_err(|_| invalid())?;
        PaymentAddress::parse(&row.source).map_err(|_| invalid())?;
        PaymentAddress::parse(&row.destination).map_err(|_| invalid())?;
        let id = (task, row.operation_index);
        if last_transfer.as_ref().is_some_and(|last| id <= *last)
            || row.amount == 0
            || (!row.incoming && !row.outgoing)
        {
            return Err(invalid());
        }
        last_transfer = Some(id);
    }
    let mut last_currency = None;
    for row in &view.currencies {
        let address = CurrencyAddress::parse(row).map_err(|_| invalid())?;
        if last_currency.is_some_and(|last| address <= last) {
            return Err(invalid());
        }
        last_currency = Some(address);
    }
    Ok(())
}

pub(super) fn append_page(view: &mut AccountView, page: AccountView) -> Result<(), NetworkError> {
    validate_page(&page)?;
    if view.account != page.account
        || view.kind != page.kind
        || view.next != Some(page.cursor)
        || view.generation != page.generation
        || view.validator_set_version != page.validator_set_version
        || view.exists != page.exists
        || view.balance != page.balance
        || view.total != page.total
    {
        return Err(NetworkError::InvalidAccountQuery);
    }
    // Both pages are already canonical. Comparing the boundary avoids rescanning accumulated rows.
    let ordered = match view.kind {
        2 => view
            .addresses
            .last()
            .zip(page.addresses.first())
            .is_none_or(|(a, b)| {
                PaymentAddress::parse(&a.address).unwrap()
                    < PaymentAddress::parse(&b.address).unwrap()
            }),
        3 => view
            .transfers
            .last()
            .zip(page.transfers.first())
            .is_none_or(|(a, b)| (&a.task_id, a.operation_index) < (&b.task_id, b.operation_index)),
        4 => view
            .currencies
            .last()
            .zip(page.currencies.first())
            .is_none_or(|(a, b)| {
                CurrencyAddress::parse(a).unwrap() < CurrencyAddress::parse(b).unwrap()
            }),
        _ => false,
    };
    if !ordered {
        return Err(NetworkError::InvalidAccountQuery);
    }
    view.addresses.extend(page.addresses);
    view.transfers.extend(page.transfers);
    view.currencies.extend(page.currencies);
    view.next = page.next;
    Ok(())
}
