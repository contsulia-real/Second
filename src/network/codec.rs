mod bft;
mod core;
mod governance;
mod legal;
mod public;
mod recovery;

use super::{
    CURRENT_NETWORK_PROTOCOL_VERSION, MAX_NETWORK_FRAME_SIZE, NetworkError, NetworkMessage,
};

const NETWORK_MAGIC: [u8; 4] = *b"SCND";
const FRAME_HEADER_SIZE: usize = 12;
pub(super) const MAX_NETWORK_MESSAGE_SIZE: usize = MAX_NETWORK_FRAME_SIZE + FRAME_HEADER_SIZE;

pub fn encode_network_message(message: &NetworkMessage) -> Result<Vec<u8>, NetworkError> {
    let payload = encode_message_payload(message)?;
    if payload.len() > MAX_NETWORK_FRAME_SIZE {
        return Err(NetworkError::FrameTooLarge {
            announced: payload.len(),
            maximum: MAX_NETWORK_FRAME_SIZE,
        });
    }
    let payload_len = u32::try_from(payload.len()).map_err(|_| NetworkError::FrameTooLarge {
        announced: payload.len(),
        maximum: MAX_NETWORK_FRAME_SIZE,
    })?;
    let mut frame = Vec::with_capacity(FRAME_HEADER_SIZE + payload.len());
    frame.extend_from_slice(&NETWORK_MAGIC);
    frame.extend_from_slice(&CURRENT_NETWORK_PROTOCOL_VERSION.to_be_bytes());
    frame.extend_from_slice(&payload_len.to_be_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

pub(super) fn network_frame_size(frame_prefix: &[u8]) -> Result<usize, NetworkError> {
    if frame_prefix.len() < FRAME_HEADER_SIZE {
        return Err(NetworkError::InvalidMessageLength {
            message_type: 0,
            expected: FRAME_HEADER_SIZE,
            actual: frame_prefix.len(),
        });
    }
    if frame_prefix[0..4] != NETWORK_MAGIC {
        return Err(NetworkError::InvalidMagic);
    }
    let protocol_version = u32::from_be_bytes(frame_prefix[4..8].try_into().map_err(|_| {
        NetworkError::InvalidMessageLength {
            message_type: 0,
            expected: FRAME_HEADER_SIZE,
            actual: frame_prefix.len(),
        }
    })?);
    if protocol_version != CURRENT_NETWORK_PROTOCOL_VERSION {
        return Err(NetworkError::UnsupportedProtocolVersion {
            expected: CURRENT_NETWORK_PROTOCOL_VERSION,
            actual: protocol_version,
        });
    }
    let announced = u32::from_be_bytes(frame_prefix[8..12].try_into().map_err(|_| {
        NetworkError::InvalidMessageLength {
            message_type: 0,
            expected: FRAME_HEADER_SIZE,
            actual: frame_prefix.len(),
        }
    })?) as usize;
    if announced > MAX_NETWORK_FRAME_SIZE {
        return Err(NetworkError::FrameTooLarge {
            announced,
            maximum: MAX_NETWORK_FRAME_SIZE,
        });
    }
    FRAME_HEADER_SIZE
        .checked_add(announced)
        .ok_or(NetworkError::FrameTooLarge {
            announced,
            maximum: MAX_NETWORK_FRAME_SIZE,
        })
}

pub fn decode_network_message(frame: &[u8]) -> Result<NetworkMessage, NetworkError> {
    let expected_len = network_frame_size(frame)?;
    if frame.len() != expected_len {
        return Err(NetworkError::InvalidMessageLength {
            message_type: 0,
            expected: expected_len,
            actual: frame.len(),
        });
    }
    decode_message_payload(&frame[FRAME_HEADER_SIZE..])
}

fn encode_message_payload(message: &NetworkMessage) -> Result<Vec<u8>, NetworkError> {
    match message {
        NetworkMessage::Hello { .. }
        | NetworkMessage::Ping { .. }
        | NetworkMessage::Pong { .. } => core::encode(message),
        NetworkMessage::GetPublicCurrencies { .. }
        | NetworkMessage::PublicCurrencies { .. }
        | NetworkMessage::GetPublicCurrencySummary
        | NetworkMessage::PublicCurrencySummary { .. }
        | NetworkMessage::GetPublicCurrencyCheckpoint
        | NetworkMessage::PublicCurrencyCheckpointProof { .. }
        | NetworkMessage::NoPublicCurrencyCheckpoint
        | NetworkMessage::GetPeers { .. }
        | NetworkMessage::Peers { .. }
        | NetworkMessage::GetPublicCurrencyDelta { .. }
        | NetworkMessage::PublicCurrencyDelta { .. }
        | NetworkMessage::NoPublicCurrencyDelta
        | NetworkMessage::GetValidatorSetTransitionProof { .. }
        | NetworkMessage::ValidatorSetTransitionProof { .. }
        | NetworkMessage::NoValidatorSetTransitionProof => public::encode(message),
        NetworkMessage::GetStateRecoveryManifest { .. }
        | NetworkMessage::StateRecoveryManifest { .. }
        | NetworkMessage::NoStateRecoveryCheckpoint
        | NetworkMessage::GetStateRecoveryChunk { .. }
        | NetworkMessage::StateRecoveryChunk { .. }
        | NetworkMessage::StateRecoveryDenied => recovery::encode(message),
        NetworkMessage::BftAuthenticate { .. }
        | NetworkMessage::BftAuthenticated { .. }
        | NetworkMessage::BftMessage { .. }
        | NetworkMessage::BftDenied => bft::encode(message),
        NetworkMessage::LegalTaskSubmissionOpen { .. }
        | NetworkMessage::LegalTaskSubmissionChunk { .. }
        | NetworkMessage::LegalTaskSubmissionContinue { .. }
        | NetworkMessage::LegalTaskSubmissionAccepted { .. }
        | NetworkMessage::LegalTaskSubmissionRejected { .. }
        | NetworkMessage::LegalTaskStatusQuery { .. }
        | NetworkMessage::LegalTaskStatusResult { .. }
        | NetworkMessage::LegalTaskStatusRejected { .. } => legal::encode(message),
        NetworkMessage::ValidatorTransitionSubmit { .. }
        | NetworkMessage::ValidatorTransitionAccepted { .. }
        | NetworkMessage::StateRecoveryCheckpointSubmit { .. }
        | NetworkMessage::StateRecoveryCheckpointAccepted { .. }
        | NetworkMessage::GovernanceRejected { .. } => governance::encode(message),
    }
}

fn decode_message_payload(payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    let message_type = *payload.first().ok_or(NetworkError::EmptyPayload)?;
    match message_type {
        1..=3 => core::decode(message_type, payload),
        4..=12 | 33..=38 => public::decode(message_type, payload),
        13..=18 => recovery::decode(message_type, payload),
        19..=22 => bft::decode(message_type, payload),
        23..=27 | 39..=41 => legal::decode(message_type, payload),
        28..=32 => governance::decode(message_type, payload),
        other => Err(NetworkError::UnknownMessageType(other)),
    }
}

pub(super) fn require_message_length(
    message_type: u8,
    payload: &[u8],
    expected: usize,
) -> Result<(), NetworkError> {
    if payload.len() == expected {
        Ok(())
    } else {
        Err(NetworkError::InvalidMessageLength {
            message_type,
            expected,
            actual: payload.len(),
        })
    }
}
