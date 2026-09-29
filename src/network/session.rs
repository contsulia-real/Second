use std::io::{Read, Write};

use crate::{CurrencyAddress, SecondState};

use super::codec::{read_network_message, read_network_message_optional, write_network_message};
use super::{
    NetworkError, NetworkMessage, NodeId, RemotePublicCurrencyPage, validate_public_currency_span,
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
    span: u16,
) -> Result<RemotePublicCurrencyPage, NetworkError> {
    validate_public_currency_span(span)?;
    let remote_node_id = client_handshake(stream, local_node_id)?;

    write_network_message(stream, &NetworkMessage::GetPublicCurrencies { start, span })?;

    match read_network_message(stream)? {
        NetworkMessage::PublicCurrencies { states, next_start } => Ok(RemotePublicCurrencyPage {
            remote_node_id,
            states,
            next_start,
        }),
        _ => Err(NetworkError::UnexpectedMessage),
    }
}

pub fn serve_public_currency_session<S: Read + Write>(
    stream: &mut S,
    local_node_id: NodeId,
    state: &SecondState,
) -> Result<NodeId, NetworkError> {
    let remote_node_id = server_handshake(stream, local_node_id)?;

    match read_network_message(stream)? {
        NetworkMessage::GetPublicCurrencies { start, span } => {
            write_public_currency_page(stream, state, start, span)?;
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
            NetworkMessage::GetPublicCurrencies { start, span } => {
                write_public_currency_page(stream, state, start, span)?;
            }
            _ => return Err(NetworkError::UnexpectedMessage),
        }
    }
}

fn write_public_currency_page<S: Write>(
    stream: &mut S,
    state: &SecondState,
    start: CurrencyAddress,
    span: u16,
) -> Result<(), NetworkError> {
    let page = state.public_currency_page(start, span)?;
    write_network_message(
        stream,
        &NetworkMessage::PublicCurrencies {
            states: page.states,
            next_start: page.next_start,
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
