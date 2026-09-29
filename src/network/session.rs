use std::io::{Read, Write};

use crate::{CurrencyAddress, PublicCurrencyView, SecondState};

use super::codec::{read_network_message, read_network_message_optional, write_network_message};
use super::{
    MAX_PUBLIC_CURRENCY_PAGE, NetworkError, NetworkMessage, NodeId, RemotePublicCurrencyPage,
    RemotePublicCurrencySummary, RemotePublicCurrencyView, validate_public_currency_limit,
};

pub fn client_ping<S: Read + Write>(
    stream: &mut S,
    local_node_id: NodeId,
    nonce: u64,
) -> Result<NodeId, NetworkError> {
    let remote_node_id = client_handshake(stream, local_node_id)?;

    write_network_message(stream, &NetworkMessage::Ping { nonce })?;

    match read_network_message(stream)? {
        NetworkMessage::Pong {
            nonce: response_nonce,
        } if response_nonce == nonce => Ok(remote_node_id),
        NetworkMessage::Pong {
            nonce: response_nonce,
        } => Err(NetworkError::NonceMismatch {
            expected: nonce,
            actual: response_nonce,
        }),
        _ => Err(NetworkError::UnexpectedMessage),
    }
}

pub fn serve_ping_session<S: Read + Write>(
    stream: &mut S,
    local_node_id: NodeId,
) -> Result<NodeId, NetworkError> {
    let remote_node_id = server_handshake(stream, local_node_id)?;

    match read_network_message(stream)? {
        NetworkMessage::Ping { nonce } => {
            write_network_message(stream, &NetworkMessage::Pong { nonce })?;
            Ok(remote_node_id)
        }
        _ => Err(NetworkError::UnexpectedMessage),
    }
}

pub fn client_public_currency_page<S: Read + Write>(
    stream: &mut S,
    local_node_id: NodeId,
    start: CurrencyAddress,
    limit: u16,
) -> Result<RemotePublicCurrencyPage, NetworkError> {
    validate_public_currency_limit(limit)?;
    let remote_node_id = client_handshake(stream, local_node_id)?;
    let (states, next_start) = request_public_currency_page(stream, start, limit)?;

    Ok(RemotePublicCurrencyPage {
        remote_node_id,
        states,
        next_start,
    })
}

pub fn client_public_currency_summary<S: Read + Write>(
    stream: &mut S,
    local_node_id: NodeId,
) -> Result<RemotePublicCurrencySummary, NetworkError> {
    let remote_node_id = client_handshake(stream, local_node_id)?;
    let summary = request_public_currency_summary(stream)?;

    Ok(RemotePublicCurrencySummary {
        remote_node_id,
        summary,
    })
}

pub fn client_sync_public_currency_view<S: Read + Write>(
    stream: &mut S,
    local_node_id: NodeId,
) -> Result<RemotePublicCurrencyView, NetworkError> {
    let remote_node_id = client_handshake(stream, local_node_id)?;
    let summary = request_public_currency_summary(stream)?;
    let mut states = Vec::new();

    if summary.current_supply > 0 {
        let mut start = CurrencyAddress::new(0);

        loop {
            let remaining = summary.current_supply.saturating_sub(states.len() as u64);
            if remaining == 0 {
                return Err(NetworkError::InvalidPublicCurrencyPage);
            }

            let limit = remaining.min(u64::from(MAX_PUBLIC_CURRENCY_PAGE)) as u16;
            let (page_states, next_start) = request_public_currency_page(stream, start, limit)?;

            validate_synced_page(
                start,
                limit,
                summary.next_currency_address,
                &page_states,
                next_start,
            )?;

            states.extend(page_states);
            let actual = states.len() as u64;
            if actual > summary.current_supply {
                return Err(NetworkError::SynchronizedCurrencyCountExceeded {
                    claimed: summary.current_supply,
                    actual,
                });
            }

            match next_start {
                Some(next) => {
                    if actual >= summary.current_supply {
                        return Err(NetworkError::InvalidPublicCurrencyPage);
                    }
                    start = next;
                }
                None => break,
            }
        }
    }

    let view = PublicCurrencyView::new(summary, states).map_err(NetworkError::PublicState)?;

    Ok(RemotePublicCurrencyView {
        remote_node_id,
        view,
    })
}

pub fn serve_public_currency_session<S: Read + Write>(
    stream: &mut S,
    local_node_id: NodeId,
    state: &SecondState,
) -> Result<NodeId, NetworkError> {
    let remote_node_id = server_handshake(stream, local_node_id)?;

    match read_network_message(stream)? {
        NetworkMessage::GetPublicCurrencies { start, limit } => {
            write_public_currency_page(stream, state, start, limit)?;
            Ok(remote_node_id)
        }
        _ => Err(NetworkError::UnexpectedMessage),
    }
}

pub fn serve_public_currency_connection<S: Read + Write>(
    stream: &mut S,
    local_node_id: NodeId,
    state: &SecondState,
) -> Result<NodeId, NetworkError> {
    let remote_node_id = server_handshake(stream, local_node_id)?;

    loop {
        let Some(message) = read_network_message_optional(stream)? else {
            return Ok(remote_node_id);
        };

        match message {
            NetworkMessage::Ping { nonce } => {
                write_network_message(stream, &NetworkMessage::Pong { nonce })?;
            }
            NetworkMessage::GetPublicCurrencies { start, limit } => {
                write_public_currency_page(stream, state, start, limit)?;
            }
            NetworkMessage::GetPublicCurrencySummary => {
                write_public_currency_summary(stream, state)?;
            }
            _ => return Err(NetworkError::UnexpectedMessage),
        }
    }
}

fn request_public_currency_page<S: Read + Write>(
    stream: &mut S,
    start: CurrencyAddress,
    limit: u16,
) -> Result<(Vec<crate::PublicCurrencyState>, Option<CurrencyAddress>), NetworkError> {
    validate_public_currency_limit(limit)?;
    write_network_message(
        stream,
        &NetworkMessage::GetPublicCurrencies { start, limit },
    )?;

    match read_network_message(stream)? {
        NetworkMessage::PublicCurrencies { states, next_start } => Ok((states, next_start)),
        _ => Err(NetworkError::UnexpectedMessage),
    }
}

fn request_public_currency_summary<S: Read + Write>(
    stream: &mut S,
) -> Result<crate::PublicCurrencySummary, NetworkError> {
    write_network_message(stream, &NetworkMessage::GetPublicCurrencySummary)?;

    match read_network_message(stream)? {
        NetworkMessage::PublicCurrencySummary { summary } => Ok(summary),
        _ => Err(NetworkError::UnexpectedMessage),
    }
}

fn validate_synced_page(
    start: CurrencyAddress,
    limit: u16,
    frontier: u64,
    states: &[crate::PublicCurrencyState],
    next_start: Option<CurrencyAddress>,
) -> Result<(), NetworkError> {
    if states.len() > usize::from(limit) {
        return Err(NetworkError::InvalidPublicCurrencyPage);
    }

    let mut previous = None;
    for state in states {
        if state.address < start {
            return Err(NetworkError::InvalidPublicCurrencyPage);
        }

        if let Some(previous_address) = previous
            && state.address <= previous_address
        {
            return Err(NetworkError::InvalidPublicCurrencyPage);
        }

        previous = Some(state.address);
    }

    if let Some(next) = next_start {
        if states.len() != usize::from(limit) {
            return Err(NetworkError::InvalidPublicCurrencyPage);
        }

        let current = states.last().map(|state| state.address).unwrap_or(start);

        if next <= current || next.value() >= frontier {
            return Err(NetworkError::InvalidPublicCurrencyCursor { current, next });
        }
    }

    Ok(())
}

fn write_public_currency_page<S: Write>(
    stream: &mut S,
    state: &SecondState,
    start: CurrencyAddress,
    limit: u16,
) -> Result<(), NetworkError> {
    let page = state.public_currency_page(start, limit)?;
    write_network_message(
        stream,
        &NetworkMessage::PublicCurrencies {
            states: page.states,
            next_start: page.next_start,
        },
    )
}

fn write_public_currency_summary<S: Write>(
    stream: &mut S,
    state: &SecondState,
) -> Result<(), NetworkError> {
    write_network_message(
        stream,
        &NetworkMessage::PublicCurrencySummary {
            summary: state.public_currency_summary(),
        },
    )
}

fn client_handshake<S: Read + Write>(
    stream: &mut S,
    local_node_id: NodeId,
) -> Result<NodeId, NetworkError> {
    write_network_message(
        stream,
        &NetworkMessage::Hello {
            node_id: local_node_id,
        },
    )?;

    match read_network_message(stream)? {
        NetworkMessage::Hello { node_id } => Ok(node_id),
        _ => Err(NetworkError::UnexpectedMessage),
    }
}

fn server_handshake<S: Read + Write>(
    stream: &mut S,
    local_node_id: NodeId,
) -> Result<NodeId, NetworkError> {
    let remote_node_id = match read_network_message(stream)? {
        NetworkMessage::Hello { node_id } => node_id,
        _ => return Err(NetworkError::UnexpectedMessage),
    };

    write_network_message(
        stream,
        &NetworkMessage::Hello {
            node_id: local_node_id,
        },
    )?;

    Ok(remote_node_id)
}
