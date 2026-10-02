use super::super::{NetworkError, NetworkMessage, NodeId};
use super::require_message_length;

pub(super) fn encode(message: &NetworkMessage) -> Result<Vec<u8>, NetworkError> {
    match message {
        NetworkMessage::Hello { node_id, signature } => {
            let mut payload = Vec::with_capacity(97);
            payload.push(1);
            payload.extend_from_slice(&node_id.to_bytes());
            payload.extend_from_slice(signature);
            Ok(payload)
        }
        NetworkMessage::Ping { nonce } => {
            let mut payload = Vec::with_capacity(9);
            payload.push(2);
            payload.extend_from_slice(&nonce.to_be_bytes());
            Ok(payload)
        }
        NetworkMessage::Pong { nonce } => {
            let mut payload = Vec::with_capacity(9);
            payload.push(3);
            payload.extend_from_slice(&nonce.to_be_bytes());
            Ok(payload)
        }
        _ => Err(NetworkError::UnexpectedMessage),
    }
}

pub(super) fn decode(message_type: u8, payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    match message_type {
        1 => decode_hello(payload),
        2 => decode_nonce_message(payload, false),
        3 => decode_nonce_message(payload, true),
        other => Err(NetworkError::UnknownMessageType(other)),
    }
}

fn decode_hello(payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    require_message_length(1, payload, 97)?;
    let mut node_id = [0_u8; 32];
    node_id.copy_from_slice(&payload[1..33]);
    let mut signature = [0_u8; 64];
    signature.copy_from_slice(&payload[33..97]);
    Ok(NetworkMessage::Hello {
        node_id: NodeId::from_bytes(node_id),
        signature,
    })
}

fn decode_nonce_message(payload: &[u8], pong: bool) -> Result<NetworkMessage, NetworkError> {
    let message_type = if pong { 3 } else { 2 };
    require_message_length(message_type, payload, 9)?;
    let nonce = u64::from_be_bytes(payload[1..9].try_into().map_err(|_| {
        NetworkError::InvalidMessageLength {
            message_type,
            expected: 9,
            actual: payload.len(),
        }
    })?);

    if pong {
        Ok(NetworkMessage::Pong { nonce })
    } else {
        Ok(NetworkMessage::Ping { nonce })
    }
}
