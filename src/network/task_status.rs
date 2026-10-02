use crate::{LegalTask, LegalTaskStatus, LegalTaskStatusRejection, TaskId};

use super::{NetworkError, NetworkMessage, QuicPeer};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteLegalTaskStatus {
    pub task_id: TaskId,
    pub status: LegalTaskStatus,
}

pub async fn client_legal_task_status(
    peer: &QuicPeer,
    task: &LegalTask,
) -> Result<RemoteLegalTaskStatus, NetworkError> {
    let task_id = task.payload().task_id();
    let request_digest = task
        .request_digest()
        .map_err(|_| NetworkError::InvalidLegalTaskStatus)?;
    match peer
        .exchange(&NetworkMessage::LegalTaskStatusQuery {
            task_id: task_id.clone(),
            request_digest,
        })
        .await?
    {
        NetworkMessage::LegalTaskStatusResult {
            task_id: response_task_id,
            status,
        } if response_task_id == task_id => Ok(RemoteLegalTaskStatus {
            task_id: response_task_id,
            status,
        }),
        NetworkMessage::LegalTaskStatusRejected { reason } => {
            Err(NetworkError::LegalTaskStatusRejected(reason))
        }
        _ => Err(NetworkError::UnexpectedMessage),
    }
}

pub(crate) const fn rejected(reason: LegalTaskStatusRejection) -> NetworkMessage {
    NetworkMessage::LegalTaskStatusRejected { reason }
}
