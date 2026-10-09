use super::super::{GovernanceRejection, NetworkError, NetworkMessage};
use super::require_message_length;
use crate::{MAX_VALIDATOR_TRANSITION_SOURCE_SIZE, ValidatorId};

pub(super) fn encode(message: &NetworkMessage) -> Result<Vec<u8>, NetworkError> {
    match message {
        NetworkMessage::ValidatorTransitionSubmit {
            validator_id,
            validator_set_version,
            source,
            signature,
        } => {
            if source.is_empty() || source.len() > MAX_VALIDATOR_TRANSITION_SOURCE_SIZE {
                return Err(NetworkError::InvalidGovernanceRequest);
            }
            let source_len =
                u16::try_from(source.len()).map_err(|_| NetworkError::InvalidGovernanceRequest)?;
            let mut payload = Vec::with_capacity(1 + 8 + 8 + 2 + source.len() + 64);
            payload.push(28);
            payload.extend_from_slice(&validator_id.value().to_be_bytes());
            payload.extend_from_slice(&validator_set_version.to_be_bytes());
            payload.extend_from_slice(&source_len.to_be_bytes());
            payload.extend_from_slice(source);
            payload.extend_from_slice(signature);
            Ok(payload)
        }
        NetworkMessage::ValidatorTransitionAccepted {
            current_validator_set_version,
            next_validator_set_version,
            transition_digest,
        } => {
            let mut payload = Vec::with_capacity(49);
            payload.push(29);
            payload.extend_from_slice(&current_validator_set_version.to_be_bytes());
            payload.extend_from_slice(&next_validator_set_version.to_be_bytes());
            payload.extend_from_slice(transition_digest);
            Ok(payload)
        }
        NetworkMessage::StateRecoveryCheckpointSubmit {
            validator_id,
            validator_set_version,
            signature,
        }
        | NetworkMessage::PublicCheckpointSubmit {
            validator_id,
            validator_set_version,
            signature,
        } => {
            let mut payload = Vec::with_capacity(81);
            payload.push(
                if matches!(message, NetworkMessage::PublicCheckpointSubmit { .. }) {
                    42
                } else {
                    30
                },
            );
            payload.extend_from_slice(&validator_id.value().to_be_bytes());
            payload.extend_from_slice(&validator_set_version.to_be_bytes());
            payload.extend_from_slice(signature);
            Ok(payload)
        }
        NetworkMessage::StateRecoveryCheckpointAccepted {
            validator_set_version,
            serial,
            checkpoint_digest,
        }
        | NetworkMessage::PublicCheckpointAccepted {
            validator_set_version,
            epoch: serial,
            checkpoint_digest,
        } => {
            let mut payload = Vec::with_capacity(49);
            payload.push(
                if matches!(message, NetworkMessage::PublicCheckpointAccepted { .. }) {
                    43
                } else {
                    31
                },
            );
            payload.extend_from_slice(&validator_set_version.to_be_bytes());
            payload.extend_from_slice(&serial.to_be_bytes());
            payload.extend_from_slice(checkpoint_digest);
            Ok(payload)
        }
        NetworkMessage::GovernanceRejected { reason } => {
            Ok(vec![32, encode_governance_rejection(*reason)])
        }
        _ => Err(NetworkError::UnexpectedMessage),
    }
}

pub(super) fn decode(message_type: u8, payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    match message_type {
        28 => decode_validator_transition_submit(payload),
        29 => decode_validator_transition_accepted(payload),
        30 | 42 => decode_checkpoint_submit(message_type, payload),
        31 | 43 => decode_checkpoint_accepted(message_type, payload),
        32 => decode_governance_rejected(payload),
        other => Err(NetworkError::UnknownMessageType(other)),
    }
}

fn decode_validator_transition_submit(payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    if payload.len() < 83 {
        return Err(NetworkError::InvalidGovernanceRequest);
    }
    let validator_id = ValidatorId::new(u64::from_be_bytes(
        payload[1..9]
            .try_into()
            .map_err(|_| NetworkError::InvalidGovernanceRequest)?,
    ));
    let validator_set_version = u64::from_be_bytes(
        payload[9..17]
            .try_into()
            .map_err(|_| NetworkError::InvalidGovernanceRequest)?,
    );
    let source_len = usize::from(u16::from_be_bytes(
        payload[17..19]
            .try_into()
            .map_err(|_| NetworkError::InvalidGovernanceRequest)?,
    ));
    if source_len == 0 || source_len > MAX_VALIDATOR_TRANSITION_SOURCE_SIZE {
        return Err(NetworkError::InvalidGovernanceRequest);
    }
    let expected_len = 19_usize
        .checked_add(source_len)
        .and_then(|len| len.checked_add(64))
        .ok_or(NetworkError::InvalidGovernanceRequest)?;
    if payload.len() != expected_len {
        return Err(NetworkError::InvalidGovernanceRequest);
    }
    let signature = payload[19 + source_len..expected_len]
        .try_into()
        .map_err(|_| NetworkError::InvalidGovernanceRequest)?;
    Ok(NetworkMessage::ValidatorTransitionSubmit {
        validator_id,
        validator_set_version,
        source: payload[19..19 + source_len].to_vec(),
        signature,
    })
}

fn decode_validator_transition_accepted(payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    require_message_length(29, payload, 49)?;
    Ok(NetworkMessage::ValidatorTransitionAccepted {
        current_validator_set_version: u64::from_be_bytes(
            payload[1..9]
                .try_into()
                .map_err(|_| NetworkError::InvalidGovernanceRequest)?,
        ),
        next_validator_set_version: u64::from_be_bytes(
            payload[9..17]
                .try_into()
                .map_err(|_| NetworkError::InvalidGovernanceRequest)?,
        ),
        transition_digest: payload[17..49]
            .try_into()
            .map_err(|_| NetworkError::InvalidGovernanceRequest)?,
    })
}

fn decode_checkpoint_submit(
    message_type: u8,
    payload: &[u8],
) -> Result<NetworkMessage, NetworkError> {
    require_message_length(message_type, payload, 81)?;
    let validator_id = ValidatorId::new(u64::from_be_bytes(payload[1..9].try_into().unwrap()));
    let validator_set_version = u64::from_be_bytes(payload[9..17].try_into().unwrap());
    let signature = payload[17..81].try_into().unwrap();
    Ok(if message_type == 42 {
        NetworkMessage::PublicCheckpointSubmit {
            validator_id,
            validator_set_version,
            signature,
        }
    } else {
        NetworkMessage::StateRecoveryCheckpointSubmit {
            validator_id,
            validator_set_version,
            signature,
        }
    })
}

fn decode_checkpoint_accepted(
    message_type: u8,
    payload: &[u8],
) -> Result<NetworkMessage, NetworkError> {
    require_message_length(message_type, payload, 49)?;
    let validator_set_version = u64::from_be_bytes(payload[1..9].try_into().unwrap());
    let sequence = u64::from_be_bytes(payload[9..17].try_into().unwrap());
    let checkpoint_digest = payload[17..49].try_into().unwrap();
    Ok(if message_type == 43 {
        NetworkMessage::PublicCheckpointAccepted {
            validator_set_version,
            epoch: sequence,
            checkpoint_digest,
        }
    } else {
        NetworkMessage::StateRecoveryCheckpointAccepted {
            validator_set_version,
            serial: sequence,
            checkpoint_digest,
        }
    })
}

fn decode_governance_rejected(payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    require_message_length(32, payload, 2)?;
    Ok(NetworkMessage::GovernanceRejected {
        reason: decode_governance_rejection(payload[1])?,
    })
}

const fn encode_governance_rejection(reason: GovernanceRejection) -> u8 {
    match reason {
        GovernanceRejection::Unavailable => 1,
        GovernanceRejection::Busy => 2,
        GovernanceRejection::Unauthorized => 3,
        GovernanceRejection::Rejected => 4,
    }
}

fn decode_governance_rejection(value: u8) -> Result<GovernanceRejection, NetworkError> {
    match value {
        1 => Ok(GovernanceRejection::Unavailable),
        2 => Ok(GovernanceRejection::Busy),
        3 => Ok(GovernanceRejection::Unauthorized),
        4 => Ok(GovernanceRejection::Rejected),
        _ => Err(NetworkError::InvalidGovernanceRequest),
    }
}
