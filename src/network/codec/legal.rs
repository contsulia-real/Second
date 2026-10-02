use super::super::{
    LegalTaskStatusRejection, LegalTaskSubmissionRejection, MAX_PREPARED_TASK_SOURCE_CHUNK_SIZE,
    NetworkError, NetworkMessage,
};
use super::require_message_length;
use crate::{LegalTaskStatus, LegalTaskSubmissionOutcome, TaskId};

pub(super) fn encode(message: &NetworkMessage) -> Result<Vec<u8>, NetworkError> {
    match message {
        NetworkMessage::LegalTaskSubmissionOpen { total_len } => {
            let mut payload = Vec::with_capacity(5);
            payload.push(23);
            payload.extend_from_slice(&total_len.to_be_bytes());
            Ok(payload)
        }
        NetworkMessage::LegalTaskSubmissionChunk { offset, bytes } => {
            if bytes.is_empty() || bytes.len() > MAX_PREPARED_TASK_SOURCE_CHUNK_SIZE {
                return Err(NetworkError::InvalidLegalTaskSubmission);
            }
            let mut payload = Vec::with_capacity(5 + bytes.len());
            payload.push(24);
            payload.extend_from_slice(&offset.to_be_bytes());
            payload.extend_from_slice(bytes);
            Ok(payload)
        }
        NetworkMessage::LegalTaskSubmissionContinue { next_offset } => {
            let mut payload = Vec::with_capacity(5);
            payload.push(25);
            payload.extend_from_slice(&next_offset.to_be_bytes());
            Ok(payload)
        }
        NetworkMessage::LegalTaskSubmissionAccepted { task_id, outcome } => {
            let mut payload = Vec::with_capacity(3 + task_id.len());
            payload.push(26);
            push_task_id(&mut payload, task_id)
                .map_err(|_| NetworkError::InvalidLegalTaskSubmission)?;
            payload.push(encode_submission_outcome(*outcome));
            Ok(payload)
        }
        NetworkMessage::LegalTaskSubmissionRejected { reason } => {
            Ok(vec![27, encode_submission_rejection(*reason)])
        }
        NetworkMessage::LegalTaskStatusQuery {
            task_id,
            request_digest,
        } => {
            let mut payload = Vec::with_capacity(34 + task_id.len());
            payload.push(39);
            push_task_id(&mut payload, task_id)
                .map_err(|_| NetworkError::InvalidLegalTaskStatus)?;
            payload.extend_from_slice(request_digest);
            Ok(payload)
        }
        NetworkMessage::LegalTaskStatusResult { task_id, status } => {
            let mut payload = Vec::with_capacity(3 + task_id.len());
            payload.push(40);
            push_task_id(&mut payload, task_id)
                .map_err(|_| NetworkError::InvalidLegalTaskStatus)?;
            payload.push(encode_legal_task_status(*status));
            Ok(payload)
        }
        NetworkMessage::LegalTaskStatusRejected { reason } => {
            Ok(vec![41, encode_legal_task_status_rejection(*reason)])
        }
        _ => Err(NetworkError::UnexpectedMessage),
    }
}

pub(super) fn decode(message_type: u8, payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    match message_type {
        23 => decode_submission_open(payload),
        24 => decode_submission_chunk(payload),
        25 => decode_submission_continue(payload),
        26 => decode_submission_accepted(payload),
        27 => decode_submission_rejected(payload),
        39 => decode_legal_task_status_query(payload),
        40 => decode_legal_task_status_result(payload),
        41 => decode_legal_task_status_rejected(payload),
        other => Err(NetworkError::UnknownMessageType(other)),
    }
}

fn decode_legal_task_status_query(payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    let (task_id, task_id_end) =
        decode_task_id(payload).map_err(|_| NetworkError::InvalidLegalTaskStatus)?;
    let expected_len = task_id_end
        .checked_add(32)
        .ok_or(NetworkError::InvalidLegalTaskStatus)?;
    if payload.len() != expected_len {
        return Err(NetworkError::InvalidLegalTaskStatus);
    }
    let request_digest = payload[task_id_end..expected_len]
        .try_into()
        .map_err(|_| NetworkError::InvalidLegalTaskStatus)?;
    Ok(NetworkMessage::LegalTaskStatusQuery {
        task_id,
        request_digest,
    })
}

fn decode_legal_task_status_result(payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    let (task_id, task_id_end) =
        decode_task_id(payload).map_err(|_| NetworkError::InvalidLegalTaskStatus)?;
    let expected_len = task_id_end
        .checked_add(1)
        .ok_or(NetworkError::InvalidLegalTaskStatus)?;
    if payload.len() != expected_len {
        return Err(NetworkError::InvalidLegalTaskStatus);
    }
    let status = decode_legal_task_status(payload[task_id_end])?;
    Ok(NetworkMessage::LegalTaskStatusResult { task_id, status })
}

fn decode_legal_task_status_rejected(payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    require_message_length(41, payload, 2)?;
    Ok(NetworkMessage::LegalTaskStatusRejected {
        reason: decode_legal_task_status_rejection(payload[1])?,
    })
}

const fn encode_legal_task_status(status: LegalTaskStatus) -> u8 {
    match status {
        LegalTaskStatus::Unknown => 1,
        LegalTaskStatus::Bound => 2,
        LegalTaskStatus::Prepared => 3,
        LegalTaskStatus::Voting => 4,
        LegalTaskStatus::Finalized => 5,
        LegalTaskStatus::Succeeded => 6,
    }
}

fn decode_legal_task_status(value: u8) -> Result<LegalTaskStatus, NetworkError> {
    match value {
        1 => Ok(LegalTaskStatus::Unknown),
        2 => Ok(LegalTaskStatus::Bound),
        3 => Ok(LegalTaskStatus::Prepared),
        4 => Ok(LegalTaskStatus::Voting),
        5 => Ok(LegalTaskStatus::Finalized),
        6 => Ok(LegalTaskStatus::Succeeded),
        _ => Err(NetworkError::InvalidLegalTaskStatus),
    }
}

const fn encode_legal_task_status_rejection(reason: LegalTaskStatusRejection) -> u8 {
    match reason {
        LegalTaskStatusRejection::Unavailable => 1,
        LegalTaskStatusRejection::Rejected => 2,
    }
}

fn decode_legal_task_status_rejection(value: u8) -> Result<LegalTaskStatusRejection, NetworkError> {
    match value {
        1 => Ok(LegalTaskStatusRejection::Unavailable),
        2 => Ok(LegalTaskStatusRejection::Rejected),
        _ => Err(NetworkError::InvalidLegalTaskStatus),
    }
}

fn decode_submission_open(payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    require_message_length(23, payload, 5)?;
    let total_len = u32::from_be_bytes(
        payload[1..5]
            .try_into()
            .map_err(|_| NetworkError::InvalidLegalTaskSubmission)?,
    );
    Ok(NetworkMessage::LegalTaskSubmissionOpen { total_len })
}

fn decode_submission_chunk(payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    if payload.len() <= 5 || payload.len() > 5 + MAX_PREPARED_TASK_SOURCE_CHUNK_SIZE {
        return Err(NetworkError::InvalidLegalTaskSubmission);
    }
    let offset = u32::from_be_bytes(
        payload[1..5]
            .try_into()
            .map_err(|_| NetworkError::InvalidLegalTaskSubmission)?,
    );
    Ok(NetworkMessage::LegalTaskSubmissionChunk {
        offset,
        bytes: payload[5..].to_vec(),
    })
}

fn decode_submission_continue(payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    require_message_length(25, payload, 5)?;
    let next_offset = u32::from_be_bytes(
        payload[1..5]
            .try_into()
            .map_err(|_| NetworkError::InvalidLegalTaskSubmission)?,
    );
    Ok(NetworkMessage::LegalTaskSubmissionContinue { next_offset })
}

fn push_task_id(payload: &mut Vec<u8>, task_id: &TaskId) -> Result<(), ()> {
    let task_id_len = u8::try_from(task_id.len()).map_err(|_| ())?;
    payload.push(task_id_len);
    payload.extend_from_slice(task_id.as_bytes());
    Ok(())
}

fn decode_task_id(payload: &[u8]) -> Result<(TaskId, usize), ()> {
    let task_id_len = usize::from(*payload.get(1).ok_or(())?);
    let end = 2_usize.checked_add(task_id_len).ok_or(())?;
    let bytes = payload.get(2..end).ok_or(())?;
    let task_id = TaskId::from_ascii_bytes(bytes).map_err(|_| ())?;
    Ok((task_id, end))
}

fn decode_submission_accepted(payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    let (task_id, task_id_end) =
        decode_task_id(payload).map_err(|_| NetworkError::InvalidLegalTaskSubmission)?;
    let expected_len = task_id_end
        .checked_add(1)
        .ok_or(NetworkError::InvalidLegalTaskSubmission)?;
    if payload.len() != expected_len {
        return Err(NetworkError::InvalidLegalTaskSubmission);
    }
    let outcome = decode_submission_outcome(payload[task_id_end])?;
    Ok(NetworkMessage::LegalTaskSubmissionAccepted { task_id, outcome })
}

fn decode_submission_rejected(payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    require_message_length(27, payload, 2)?;
    Ok(NetworkMessage::LegalTaskSubmissionRejected {
        reason: decode_submission_rejection(payload[1])?,
    })
}

const fn encode_submission_outcome(outcome: LegalTaskSubmissionOutcome) -> u8 {
    match outcome {
        LegalTaskSubmissionOutcome::Prepared => 1,
        LegalTaskSubmissionOutcome::AlreadyPending => 2,
        LegalTaskSubmissionOutcome::AlreadySucceeded => 3,
    }
}

fn decode_submission_outcome(value: u8) -> Result<LegalTaskSubmissionOutcome, NetworkError> {
    match value {
        1 => Ok(LegalTaskSubmissionOutcome::Prepared),
        2 => Ok(LegalTaskSubmissionOutcome::AlreadyPending),
        3 => Ok(LegalTaskSubmissionOutcome::AlreadySucceeded),
        _ => Err(NetworkError::InvalidLegalTaskSubmission),
    }
}

const fn encode_submission_rejection(reason: LegalTaskSubmissionRejection) -> u8 {
    match reason {
        LegalTaskSubmissionRejection::Unavailable => 1,
        LegalTaskSubmissionRejection::Busy => 2,
        LegalTaskSubmissionRejection::Rejected => 3,
    }
}

fn decode_submission_rejection(value: u8) -> Result<LegalTaskSubmissionRejection, NetworkError> {
    match value {
        1 => Ok(LegalTaskSubmissionRejection::Unavailable),
        2 => Ok(LegalTaskSubmissionRejection::Busy),
        3 => Ok(LegalTaskSubmissionRejection::Rejected),
        _ => Err(NetworkError::InvalidLegalTaskSubmission),
    }
}
