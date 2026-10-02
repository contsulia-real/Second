use crate::legal_task_codec::{MAX_ENCODED_LEGAL_TASK_SIZE, encode_legal_task};
use crate::{LegalTask, LegalTaskSubmissionOutcome, TaskId};

use super::{
    LegalTaskSubmissionRejection, MAX_PREPARED_TASK_SOURCE_CHUNK_SIZE, NetworkError,
    NetworkMessage, QuicPeer,
};

pub const MAX_LEGAL_TASK_SUBMISSION_SIZE: usize = MAX_ENCODED_LEGAL_TASK_SIZE;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteLegalTaskSubmission {
    pub task_id: TaskId,
    pub outcome: LegalTaskSubmissionOutcome,
}

pub async fn client_submit_legal_task(
    peer: &QuicPeer,
    task: &LegalTask,
) -> Result<RemoteLegalTaskSubmission, NetworkError> {
    let encoded = encode_legal_task(task).map_err(|_| NetworkError::InvalidLegalTaskSubmission)?;
    if encoded.is_empty() || encoded.len() > MAX_LEGAL_TASK_SUBMISSION_SIZE {
        return Err(NetworkError::InvalidLegalTaskSubmission);
    }
    let total_len =
        u32::try_from(encoded.len()).map_err(|_| NetworkError::InvalidLegalTaskSubmission)?;

    match peer
        .exchange(&NetworkMessage::LegalTaskSubmissionOpen { total_len })
        .await?
    {
        NetworkMessage::LegalTaskSubmissionContinue { next_offset: 0 } => {}
        NetworkMessage::LegalTaskSubmissionRejected { reason } => {
            return Err(NetworkError::LegalTaskSubmissionRejected(reason));
        }
        _ => return Err(NetworkError::UnexpectedMessage),
    }

    let mut offset = 0_usize;
    while offset < encoded.len() {
        let end = offset
            .saturating_add(MAX_PREPARED_TASK_SOURCE_CHUNK_SIZE)
            .min(encoded.len());
        let offset_u32 =
            u32::try_from(offset).map_err(|_| NetworkError::InvalidLegalTaskSubmission)?;
        let response = peer
            .exchange(&NetworkMessage::LegalTaskSubmissionChunk {
                offset: offset_u32,
                bytes: encoded[offset..end].to_vec(),
            })
            .await?;
        offset = end;

        match response {
            NetworkMessage::LegalTaskSubmissionContinue { next_offset }
                if usize::try_from(next_offset).ok() == Some(offset) && offset < encoded.len() => {}
            NetworkMessage::LegalTaskSubmissionAccepted { task_id, outcome }
                if offset == encoded.len() && task_id == task.payload().task_id() =>
            {
                return Ok(RemoteLegalTaskSubmission { task_id, outcome });
            }
            NetworkMessage::LegalTaskSubmissionRejected { reason } => {
                return Err(NetworkError::LegalTaskSubmissionRejected(reason));
            }
            _ => return Err(NetworkError::UnexpectedMessage),
        }
    }

    Err(NetworkError::UnexpectedMessage)
}

pub(crate) fn validate_submission_open(total_len: u32) -> Result<usize, NetworkError> {
    let total_len =
        usize::try_from(total_len).map_err(|_| NetworkError::InvalidLegalTaskSubmission)?;
    if total_len == 0 || total_len > MAX_LEGAL_TASK_SUBMISSION_SIZE {
        return Err(NetworkError::InvalidLegalTaskSubmission);
    }
    Ok(total_len)
}

pub(crate) fn validate_submission_chunk(
    expected_offset: usize,
    total_len: usize,
    offset: u32,
    bytes: &[u8],
) -> Result<usize, NetworkError> {
    let offset = usize::try_from(offset).map_err(|_| NetworkError::InvalidLegalTaskSubmission)?;
    if offset != expected_offset
        || bytes.is_empty()
        || bytes.len() > MAX_PREPARED_TASK_SOURCE_CHUNK_SIZE
    {
        return Err(NetworkError::InvalidLegalTaskSubmission);
    }
    let next = offset
        .checked_add(bytes.len())
        .ok_or(NetworkError::InvalidLegalTaskSubmission)?;
    if next > total_len {
        return Err(NetworkError::InvalidLegalTaskSubmission);
    }
    Ok(next)
}

pub(crate) const fn rejected(reason: LegalTaskSubmissionRejection) -> NetworkMessage {
    NetworkMessage::LegalTaskSubmissionRejected { reason }
}
