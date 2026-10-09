use super::super::{NetworkError, NetworkMessage};
use super::require_message_length;
use crate::ValidatorId;

pub(super) fn encode(message: &NetworkMessage) -> Result<Vec<u8>, NetworkError> {
    match message {
        NetworkMessage::BftAuthenticate {
            validator_id,
            validator_set_version,
            signature,
        } => {
            let mut payload = Vec::with_capacity(81);
            payload.push(19);
            payload.extend_from_slice(&validator_id.value().to_be_bytes());
            payload.extend_from_slice(&validator_set_version.to_be_bytes());
            payload.extend_from_slice(signature);
            Ok(payload)
        }
        NetworkMessage::BftAuthenticated {
            validator_id,
            validator_set_version,
            signature,
        } => {
            let mut payload = Vec::with_capacity(81);
            payload.push(20);
            payload.extend_from_slice(&validator_id.value().to_be_bytes());
            payload.extend_from_slice(&validator_set_version.to_be_bytes());
            payload.extend_from_slice(signature);
            Ok(payload)
        }
        NetworkMessage::BftMessage { bytes } => {
            if bytes.is_empty() {
                return Err(NetworkError::InvalidBftMessage);
            }
            let mut payload = Vec::with_capacity(1 + bytes.len());
            payload.push(21);
            payload.extend_from_slice(bytes);
            Ok(payload)
        }
        NetworkMessage::BftDenied => Ok(vec![22]),
        _ => Err(NetworkError::UnexpectedMessage),
    }
}

pub(super) fn decode(message_type: u8, payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    match message_type {
        19 => decode_bft_authenticate(payload),
        20 => decode_bft_authenticated(payload),
        21 => {
            if payload.len() <= 1 {
                return Err(NetworkError::InvalidBftMessage);
            }
            Ok(NetworkMessage::BftMessage {
                bytes: payload[1..].to_vec(),
            })
        }
        22 => {
            require_message_length(22, payload, 1)?;
            Ok(NetworkMessage::BftDenied)
        }
        other => Err(NetworkError::UnknownMessageType(other)),
    }
}

fn decode_bft_authenticate(payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    require_message_length(19, payload, 81)?;
    let validator_id = ValidatorId::new(u64::from_be_bytes(
        payload[1..9]
            .try_into()
            .map_err(|_| NetworkError::InvalidBftMessage)?,
    ));
    let validator_set_version = u64::from_be_bytes(
        payload[9..17]
            .try_into()
            .map_err(|_| NetworkError::InvalidBftMessage)?,
    );
    let signature = payload[17..81]
        .try_into()
        .map_err(|_| NetworkError::InvalidBftMessage)?;
    Ok(NetworkMessage::BftAuthenticate {
        validator_id,
        validator_set_version,
        signature,
    })
}

fn decode_bft_authenticated(payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    require_message_length(20, payload, 81)?;
    let validator_id = ValidatorId::new(u64::from_be_bytes(
        payload[1..9]
            .try_into()
            .map_err(|_| NetworkError::InvalidBftMessage)?,
    ));
    let validator_set_version = u64::from_be_bytes(
        payload[9..17]
            .try_into()
            .map_err(|_| NetworkError::InvalidBftMessage)?,
    );
    let signature = payload[17..81]
        .try_into()
        .map_err(|_| NetworkError::InvalidBftMessage)?;
    Ok(NetworkMessage::BftAuthenticated {
        validator_id,
        validator_set_version,
        signature,
    })
}
