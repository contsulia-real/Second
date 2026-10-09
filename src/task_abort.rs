//! A mutually exclusive result in the existing PreparedTask consensus scope.
use crate::{CURRENT_PROTOCOL_VERSION, FinalityStatement, TaskId, ValidatorSet};
use sha2::{Digest, Sha256};

// Transient hashing context, not another durable committee or result authority.
// Snapshot validation can reuse the committee prefix for all cancelled bindings.
pub(crate) struct StatementContext {
    version: u64,
    prefix: Sha256,
}

impl StatementContext {
    pub(crate) fn new(validators: &ValidatorSet) -> Self {
        let mut prefix = Sha256::new();
        prefix.update(b"SECOND_TASK_ABORT_V1\0");
        prefix.update(CURRENT_PROTOCOL_VERSION.to_be_bytes());
        prefix.update(validators.version().to_be_bytes());
        prefix.update((validators.len() as u64).to_be_bytes());
        for credential in validators.credentials() {
            prefix.update(credential.id().value().to_be_bytes());
            prefix.update(credential.identity_public_key());
            prefix.update(credential.consensus_public_key());
            prefix.update(credential.recovery_public_key());
        }
        Self {
            version: validators.version(),
            prefix,
        }
    }

    pub(crate) fn statement(
        &self,
        task_id: &TaskId,
        request_digest: [u8; 32],
    ) -> FinalityStatement {
        let mut hash = self.prefix.clone();
        hash.update((task_id.len() as u32).to_be_bytes());
        hash.update(task_id.as_bytes());
        hash.update(request_digest);
        FinalityStatement::new(
            CURRENT_PROTOCOL_VERSION,
            self.version,
            hash.finalize().into(),
        )
    }
}

pub(crate) fn statement(
    task_id: &TaskId,
    request_digest: [u8; 32],
    validators: &ValidatorSet,
) -> FinalityStatement {
    StatementContext::new(validators).statement(task_id, request_digest)
}
