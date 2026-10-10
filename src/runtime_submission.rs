use crate::legal_task_codec::{MAX_ENCODED_LEGAL_TASK_SIZE, decode_legal_task, encode_legal_task};
use crate::network::{
    LegalTaskSubmissionRejection, NetworkError, NetworkMessage, QuicPeer, QuicRequestStream,
    legal_task_submission_rejected, validate_submission_chunk, validate_submission_open,
};
use crate::runtime_bft::ValidatorBftRuntime;
use crate::runtime_bft_consensus::start_prepared_task_consensus_for;
use crate::{
    LegalTask, NodeRuntime, NodeRuntimeError, PersistedNodeState, PreparationError,
    PreparationOutcome, PreparedTaskBook, StateStore, VerifiedLegalTask,
};

#[cfg(test)]
mod network_tests;
#[cfg(test)]
mod source_budget_tests;
#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LegalTaskSubmissionOutcome {
    Allocating,
    Prepared,
    AlreadyPending,
    AlreadySucceeded,
}

#[derive(Clone)]
pub(crate) struct LegalTaskSubmissionContext {
    store: StateStore,
    runtime: ValidatorBftRuntime,
}

impl LegalTaskSubmissionContext {
    fn new(store: StateStore, runtime: ValidatorBftRuntime) -> Self {
        Self { store, runtime }
    }

    pub(crate) fn submit(
        &self,
        task: LegalTask,
    ) -> Result<LegalTaskSubmissionOutcome, NodeRuntimeError> {
        self.runtime.refresh_authority()?;

        let verified = task.verify(self.runtime.authorizers())?;
        let source_len = encode_legal_task(&task)?.len();
        if source_len > MAX_ENCODED_LEGAL_TASK_SIZE {
            return Err(NodeRuntimeError::PreparedTaskSourceTooLarge {
                maximum: MAX_ENCODED_LEGAL_TASK_SIZE,
                actual: source_len,
            });
        }

        let persisted = self
            .store
            .load_shared()?
            .ok_or(NodeRuntimeError::SnapshotMissing)?;
        persisted
            .state
            .authorize_task(&verified)
            .map_err(PreparationError::Execution)?;
        self.submit_verified(&task, &verified, persisted)
    }

    pub(crate) fn submit_verified(
        &self,
        task: &LegalTask,
        verified: &VerifiedLegalTask,
        mut persisted: std::sync::Arc<PersistedNodeState>,
    ) -> Result<LegalTaskSubmissionOutcome, NodeRuntimeError> {
        const MAX_STALE_RETRIES: usize = 3;
        for attempt in 0..=MAX_STALE_RETRIES {
            match self.submit_snapshot(task, verified, &persisted) {
                Err(
                    NodeRuntimeError::Persistence(
                        crate::PersistenceError::StaleState
                        | crate::PersistenceError::StalePreparedTasks,
                    )
                    | NodeRuntimeError::Preparation(PreparationError::Persistence(
                        crate::PersistenceError::StaleState
                        | crate::PersistenceError::StalePreparedTasks,
                    ))
                    | NodeRuntimeError::BftConsensus(crate::BftConsensusRuntimeError::Persistence(
                        crate::PersistenceError::StaleState
                        | crate::PersistenceError::StalePreparedTasks,
                    )),
                ) if attempt < MAX_STALE_RETRIES => {
                    self.runtime.refresh_authority()?;
                    persisted = self
                        .store
                        .load_shared()?
                        .ok_or(NodeRuntimeError::SnapshotMissing)?;
                }
                result => return result,
            }
        }
        unreachable!("bounded submission retry loop always returns")
    }

    fn submit_snapshot(
        &self,
        task: &LegalTask,
        verified: &VerifiedLegalTask,
        persisted: &PersistedNodeState,
    ) -> Result<LegalTaskSubmissionOutcome, NodeRuntimeError> {
        let task_id = verified.task_id();

        if persisted.state.task_cancelled(task_id.clone()) {
            persisted
                .state
                .clone()
                .bind_task(verified)
                .map_err(PreparationError::Execution)?;
        }
        if self.existing_pending(persisted, task, verified)? {
            start_prepared_task_consensus_for(&self.store, &self.runtime, task_id)?;
            return Ok(LegalTaskSubmissionOutcome::AlreadyPending);
        }

        if crate::currency_allocation::required_count(verified)
            .map_err(PreparationError::Execution)?
            > 0
            && persisted.state.task_succeeded(task_id.clone()) != Some(true)
            && persisted
                .state
                .protocol
                .task_bindings
                .get(&task_id)
                .is_none_or(|binding| binding.allocation.is_none())
        {
            if verified.is_expired(self.runtime.now()) {
                let mut state = persisted.state.clone();
                PreparedTaskBook::from_tasks(self.store.clone(), persisted.prepared_tasks.clone())?
                    .prepare(
                        &mut state,
                        verified,
                        self.runtime.now(),
                        &persisted.validator_set,
                    )?;
                return Err(PreparationError::Execution(crate::ExecutionError::TaskExpired).into());
            }
            let already_pending = persisted
                .state
                .protocol
                .task_bindings
                .get(&task_id)
                .is_some_and(|binding| binding.allocation_task.as_ref() == Some(task));
            let allocation = crate::CurrencyAllocation::new(
                verified,
                persisted.validator_set.version(),
                persisted.state.next_currency_address(),
            )
            .map_err(PreparationError::Execution)?;
            // Validate the range before persisting its restart work item.
            // Exact durable retries already have that work item. Registration
            // rechecks the current frontier; stale snapshots use the common retry.
            if !already_pending {
                self.store.queue_currency_allocation(verified)?;
            }
            crate::runtime_bft_consensus::start_validator_consensus_target_for(
                &self.store,
                &self.runtime,
                crate::runtime_consensus_target::ValidatorConsensusTarget::CurrencyAllocation(
                    allocation,
                ),
            )?;
            return Ok(if already_pending {
                LegalTaskSubmissionOutcome::AlreadyPending
            } else {
                LegalTaskSubmissionOutcome::Allocating
            });
        }

        let mut state = persisted.state.clone();
        let validator_set = &persisted.validator_set;
        let mut prepared =
            PreparedTaskBook::from_tasks(self.store.clone(), persisted.prepared_tasks.clone())?;
        match prepared.prepare(&mut state, verified, self.runtime.now(), validator_set) {
            Ok(PreparationOutcome::Prepared) => {
                start_prepared_task_consensus_for(&self.store, &self.runtime, task_id)?;
                Ok(LegalTaskSubmissionOutcome::Prepared)
            }
            Ok(PreparationOutcome::AlreadySucceeded) => {
                Ok(LegalTaskSubmissionOutcome::AlreadySucceeded)
            }
            Err(PreparationError::AlreadyPrepared(_)) => {
                let reloaded = self
                    .store
                    .load()?
                    .ok_or(NodeRuntimeError::SnapshotMissing)?;
                if self.existing_pending(&reloaded, task, verified)? {
                    start_prepared_task_consensus_for(&self.store, &self.runtime, task_id)?;
                    Ok(LegalTaskSubmissionOutcome::AlreadyPending)
                } else {
                    Err(PreparationError::AlreadyPrepared(task_id).into())
                }
            }
            Err(
                PreparationError::Claim(
                    crate::ClaimError::CurrencyContention { .. }
                    | crate::ClaimError::ExplicitCurrencyContention { .. }
                    | crate::ClaimError::ReserveContention { .. },
                )
                | PreparationError::AccountContention(_)
                | PreparationError::PaymentAddressContention(_)
                | PreparationError::CertifiedResourceFence { .. },
            ) => {
                let contention = prepared.verify_local_contention(
                    &state,
                    verified,
                    self.runtime.now(),
                    validator_set,
                )?;
                let losers =
                    prepared.admit_contention(&mut state, verified, validator_set, contention)?;
                for loser in losers {
                    start_prepared_task_consensus_for(&self.store, &self.runtime, loser)?;
                }
                Ok(LegalTaskSubmissionOutcome::AlreadyPending)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn existing_pending(
        &self,
        persisted: &PersistedNodeState,
        task: &LegalTask,
        verified: &VerifiedLegalTask,
    ) -> Result<bool, NodeRuntimeError> {
        let task_id = verified.task_id();
        let Some(existing) = persisted.prepared_tasks.get(&task_id) else {
            return Ok(false);
        };
        if existing.request_digest == verified.request_digest() && existing.source_task == *task {
            Ok(true)
        } else {
            Err(PreparationError::AlreadyPrepared(task_id).into())
        }
    }
}

impl NodeRuntime {
    pub fn submit_legal_task(
        &self,
        task: LegalTask,
    ) -> Result<LegalTaskSubmissionOutcome, NodeRuntimeError> {
        self.task_submission_context()
            .ok_or(NodeRuntimeError::ValidatorBftNotConfigured)?
            .submit(task)
    }

    pub(crate) fn task_submission_context(&self) -> Option<LegalTaskSubmissionContext> {
        let store = self.full_store().ok()?.clone();
        self.validator_bft
            .clone()
            .map(|runtime| LegalTaskSubmissionContext::new(store, runtime))
    }
}

pub(crate) async fn serve_legal_task_submission_from_request(
    context: LegalTaskSubmissionContext,
    peer: &QuicPeer,
    first_request: QuicRequestStream,
    submission_permit: crate::runtime::ActiveConnectionPermit,
) -> Result<(), NetworkError> {
    let total_len = match first_request.message() {
        NetworkMessage::LegalTaskSubmissionOpen { total_len } => *total_len,
        _ => return Err(NetworkError::UnexpectedMessage),
    };
    let total_len = match validate_submission_open(total_len) {
        Ok(total_len) => total_len,
        Err(_) => {
            first_request
                .respond(&legal_task_submission_rejected(
                    LegalTaskSubmissionRejection::Rejected,
                ))
                .await?;
            return Ok(());
        }
    };

    let mut source = Vec::new();
    if source.try_reserve_exact(total_len).is_err() {
        first_request
            .respond(&legal_task_submission_rejected(
                LegalTaskSubmissionRejection::Busy,
            ))
            .await?;
        return Ok(());
    }
    first_request
        .respond(&NetworkMessage::LegalTaskSubmissionContinue { next_offset: 0 })
        .await?;

    while source.len() < total_len {
        let Some(request) = peer.accept_request().await? else {
            return Err(NetworkError::UnexpectedMessage);
        };
        let (offset, bytes) = match request.message() {
            NetworkMessage::LegalTaskSubmissionChunk { offset, bytes } => (*offset, bytes.clone()),
            _ => {
                request
                    .respond(&legal_task_submission_rejected(
                        LegalTaskSubmissionRejection::Rejected,
                    ))
                    .await?;
                return Ok(());
            }
        };
        let next = match validate_submission_chunk(source.len(), total_len, offset, &bytes) {
            Ok(next) => next,
            Err(_) => {
                request
                    .respond(&legal_task_submission_rejected(
                        LegalTaskSubmissionRejection::Rejected,
                    ))
                    .await?;
                return Ok(());
            }
        };
        source.extend_from_slice(&bytes);

        if next < total_len {
            let next_offset =
                u32::try_from(next).map_err(|_| NetworkError::InvalidLegalTaskSubmission)?;
            request
                .respond(&NetworkMessage::LegalTaskSubmissionContinue { next_offset })
                .await?;
            continue;
        }

        let Some(task) = decode_legal_task(&source) else {
            request
                .respond(&legal_task_submission_rejected(
                    LegalTaskSubmissionRejection::Rejected,
                ))
                .await?;
            return Ok(());
        };
        let task_id = task.payload().task_id();
        let outcome = tokio::task::spawn_blocking(move || {
            // Keep admission bounded even if the network request is cancelled
            // while its already authenticated transaction is waiting on storage.
            let _permit = submission_permit;
            context.submit(task)
        })
        .await
        .map_err(|error| NetworkError::Transport(format!("submission worker failed: {error}")))?;
        let response = match outcome {
            Ok(outcome) => NetworkMessage::LegalTaskSubmissionAccepted { task_id, outcome },
            Err(_) => legal_task_submission_rejected(LegalTaskSubmissionRejection::Rejected),
        };
        request.respond(&response).await?;
        return Ok(());
    }

    Err(NetworkError::InvalidLegalTaskSubmission)
}
