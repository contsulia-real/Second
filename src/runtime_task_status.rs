use crate::network::{NetworkError, NetworkMessage, QuicRequestStream, legal_task_status_rejected};
use crate::prepared_plan::PreparedTaskPhase;
use crate::{
    LegalTaskStatus, LegalTaskStatusRejection, NodeRuntime, NodeRuntimeError, StateStore, TaskId,
};

#[derive(Clone)]
pub(crate) struct LegalTaskStatusContext {
    store: StateStore,
}

impl LegalTaskStatusContext {
    fn new(store: StateStore) -> Self {
        Self { store }
    }

    pub(crate) fn status(
        &self,
        task_id: TaskId,
        request_digest: [u8; 32],
    ) -> Result<LegalTaskStatus, NodeRuntimeError> {
        let persisted = self
            .store
            .load()?
            .ok_or(NodeRuntimeError::SnapshotMissing)?;

        if persisted.state.bound_request_digest(task_id.clone()) != Some(request_digest) {
            return Ok(LegalTaskStatus::Unknown);
        }
        if persisted.state.task_succeeded(task_id.clone()) == Some(true) {
            return Ok(LegalTaskStatus::Succeeded);
        }

        let Some(prepared) = persisted.prepared_tasks.get(&task_id) else {
            return Ok(LegalTaskStatus::Bound);
        };
        if prepared.request_digest != request_digest {
            return Ok(LegalTaskStatus::Unknown);
        }

        Ok(match prepared.phase {
            PreparedTaskPhase::Prepared => LegalTaskStatus::Prepared,
            PreparedTaskPhase::Voting => LegalTaskStatus::Voting,
            PreparedTaskPhase::Finalized => LegalTaskStatus::Finalized,
        })
    }
}

impl NodeRuntime {
    pub(crate) fn task_status_context(&self) -> Option<LegalTaskStatusContext> {
        Some(LegalTaskStatusContext::new(self.full_store().ok()?.clone()))
    }
}

pub(crate) async fn serve_legal_task_status_from_request(
    context: LegalTaskStatusContext,
    first_request: QuicRequestStream,
) -> Result<(), NetworkError> {
    let (task_id, request_digest) = match first_request.message() {
        NetworkMessage::LegalTaskStatusQuery {
            task_id,
            request_digest,
        } => (task_id.clone(), *request_digest),
        _ => return Err(NetworkError::UnexpectedMessage),
    };

    let response = match context.status(task_id.clone(), request_digest) {
        Ok(status) => NetworkMessage::LegalTaskStatusResult { task_id, status },
        Err(_) => legal_task_status_rejected(LegalTaskStatusRejection::Rejected),
    };
    first_request.respond(&response).await
}
