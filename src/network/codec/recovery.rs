use super::super::{
    MAX_STATE_RECOVERY_CHUNK_SIZE, NetworkError, NetworkMessage, validate_chunk_limit,
};
use super::require_message_length;
use crate::{
    StateRecoveryCheckpointProof, ValidatorId,
    state_recovery_checkpoint::StateRecoveryProofCodecError,
};

pub(super) fn encode(message: &NetworkMessage) -> Result<Vec<u8>, NetworkError> {
    match message {
        NetworkMessage::GetStateRecoveryManifest {
            validator_id,
            signature,
        } => {
            let mut payload = Vec::with_capacity(73);
            payload.push(13);
            payload.extend_from_slice(&validator_id.value().to_be_bytes());
            payload.extend_from_slice(signature);
            Ok(payload)
        }
        NetworkMessage::StateRecoveryManifest { proof, payload_len } => {
            let proof = proof
                .encode_bytes()
                .map_err(|_| NetworkError::InvalidStateRecoveryProof)?;
            let mut payload = Vec::with_capacity(9 + proof.len());
            payload.push(14);
            payload.extend_from_slice(&payload_len.to_be_bytes());
            payload.extend_from_slice(&proof);
            Ok(payload)
        }
        NetworkMessage::NoStateRecoveryCheckpoint => Ok(vec![15]),
        NetworkMessage::GetStateRecoveryChunk {
            validator_id,
            checkpoint_digest,
            offset,
            limit,
            signature,
        } => {
            validate_chunk_limit(*limit)?;
            let mut payload = Vec::with_capacity(117);
            payload.push(16);
            payload.extend_from_slice(&validator_id.value().to_be_bytes());
            payload.extend_from_slice(checkpoint_digest);
            payload.extend_from_slice(&offset.to_be_bytes());
            payload.extend_from_slice(&limit.to_be_bytes());
            payload.extend_from_slice(signature);
            Ok(payload)
        }
        NetworkMessage::StateRecoveryChunk {
            checkpoint_digest,
            offset,
            bytes,
        } => {
            if bytes.is_empty() || bytes.len() > MAX_STATE_RECOVERY_CHUNK_SIZE as usize {
                return Err(NetworkError::InvalidStateRecoveryChunk);
            }
            let len =
                u32::try_from(bytes.len()).map_err(|_| NetworkError::InvalidStateRecoveryChunk)?;
            let mut payload = Vec::with_capacity(45 + bytes.len());
            payload.push(17);
            payload.extend_from_slice(checkpoint_digest);
            payload.extend_from_slice(&offset.to_be_bytes());
            payload.extend_from_slice(&len.to_be_bytes());
            payload.extend_from_slice(bytes);
            Ok(payload)
        }
        NetworkMessage::StateRecoveryDenied => Ok(vec![18]),
        _ => Err(NetworkError::UnexpectedMessage),
    }
}

pub(super) fn decode(message_type: u8, payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    match message_type {
        13 => decode_state_recovery_manifest_request(payload),
        14 => decode_state_recovery_manifest(payload),
        15 => {
            require_message_length(15, payload, 1)?;
            Ok(NetworkMessage::NoStateRecoveryCheckpoint)
        }
        16 => decode_state_recovery_chunk_request(payload),
        17 => decode_state_recovery_chunk(payload),
        18 => {
            require_message_length(18, payload, 1)?;
            Ok(NetworkMessage::StateRecoveryDenied)
        }
        other => Err(NetworkError::UnknownMessageType(other)),
    }
}

fn decode_state_recovery_manifest_request(payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    require_message_length(13, payload, 73)?;
    let validator_id = ValidatorId::new(u64::from_be_bytes(
        payload[1..9]
            .try_into()
            .map_err(|_| NetworkError::InvalidStateRecoveryProof)?,
    ));
    let signature = payload[9..73]
        .try_into()
        .map_err(|_| NetworkError::InvalidStateRecoveryProof)?;
    Ok(NetworkMessage::GetStateRecoveryManifest {
        validator_id,
        signature,
    })
}

fn decode_state_recovery_manifest(payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    if payload.len() <= 9 {
        return Err(NetworkError::InvalidStateRecoveryProof);
    }
    let payload_len = u64::from_be_bytes(
        payload[1..9]
            .try_into()
            .map_err(|_| NetworkError::InvalidStateRecoveryProof)?,
    );
    let proof =
        StateRecoveryCheckpointProof::decode_bytes(&payload[9..]).map_err(|error| match error {
            StateRecoveryProofCodecError::LengthOverflow
            | StateRecoveryProofCodecError::InvalidLength => {
                NetworkError::InvalidStateRecoveryProof
            }
        })?;
    Ok(NetworkMessage::StateRecoveryManifest { proof, payload_len })
}

fn decode_state_recovery_chunk_request(payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    require_message_length(16, payload, 117)?;
    let validator_id = ValidatorId::new(u64::from_be_bytes(
        payload[1..9]
            .try_into()
            .map_err(|_| NetworkError::InvalidStateRecoveryChunk)?,
    ));
    let checkpoint_digest = payload[9..41]
        .try_into()
        .map_err(|_| NetworkError::InvalidStateRecoveryChunk)?;
    let offset = u64::from_be_bytes(
        payload[41..49]
            .try_into()
            .map_err(|_| NetworkError::InvalidStateRecoveryChunk)?,
    );
    let limit = u32::from_be_bytes(
        payload[49..53]
            .try_into()
            .map_err(|_| NetworkError::InvalidStateRecoveryChunk)?,
    );
    validate_chunk_limit(limit)?;
    let signature = payload[53..117]
        .try_into()
        .map_err(|_| NetworkError::InvalidStateRecoveryChunk)?;
    Ok(NetworkMessage::GetStateRecoveryChunk {
        validator_id,
        checkpoint_digest,
        offset,
        limit,
        signature,
    })
}

fn decode_state_recovery_chunk(payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    if payload.len() < 45 {
        return Err(NetworkError::InvalidStateRecoveryChunk);
    }
    let checkpoint_digest = payload[1..33]
        .try_into()
        .map_err(|_| NetworkError::InvalidStateRecoveryChunk)?;
    let offset = u64::from_be_bytes(
        payload[33..41]
            .try_into()
            .map_err(|_| NetworkError::InvalidStateRecoveryChunk)?,
    );
    let len = usize::try_from(u32::from_be_bytes(
        payload[41..45]
            .try_into()
            .map_err(|_| NetworkError::InvalidStateRecoveryChunk)?,
    ))
    .map_err(|_| NetworkError::InvalidStateRecoveryChunk)?;
    if len == 0 || len > MAX_STATE_RECOVERY_CHUNK_SIZE as usize {
        return Err(NetworkError::InvalidStateRecoveryChunk);
    }
    let expected = 45_usize
        .checked_add(len)
        .ok_or(NetworkError::InvalidStateRecoveryChunk)?;
    if payload.len() != expected {
        return Err(NetworkError::InvalidStateRecoveryChunk);
    }
    Ok(NetworkMessage::StateRecoveryChunk {
        checkpoint_digest,
        offset,
        bytes: payload[45..].to_vec(),
    })
}
