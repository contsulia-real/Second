use std::collections::{BTreeMap, BTreeSet};

use crate::legal_task_codec::MAX_ENCODED_LEGAL_TASK_SIZE;
use crate::legal_task_codec::encode_legal_task;
use crate::network::MAX_PREPARED_TASK_SOURCE_CHUNK_SIZE;
use crate::runtime_bft::ValidatorBftRuntime;
use crate::runtime_bft_consensus::MAX_PENDING_UNREGISTERED_SCOPES;
use crate::{BftNetworkMessage, ConsensusScope, NetworkError, ValidatorId, ValidatorSet};

pub(crate) const MAX_PREPARED_TASK_SOURCE_SIZE: usize = MAX_ENCODED_LEGAL_TASK_SIZE;

#[derive(Default)]
pub(super) struct PreparedTaskSyncState {
    fetches: BTreeMap<ConsensusScope, PreparedTaskFetch>,
    announcements: BTreeMap<ConsensusScope, PreparedTaskAnnouncement>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PreparedTaskAnnouncement {
    validator_set_version: u64,
    scope: ConsensusScope,
    expected_plan_digest: [u8; 32],
    round: u64,
    proposer: ValidatorId,
}

pub(crate) enum PreparedTaskChunkResult {
    Pending,
    Complete(Vec<u8>),
    Reject,
}

pub(crate) struct PreparedTaskChunk {
    pub sender: ValidatorId,
    pub validator_set_version: u64,
    pub scope: ConsensusScope,
    pub expected_plan_digest: [u8; 32],
    pub total_len: u64,
    pub offset: u64,
    pub bytes: Vec<u8>,
}

struct PreparedTaskFetch {
    validator_set_version: u64,
    scope: ConsensusScope,
    expected_plan_digest: [u8; 32],
    announced_round: u64,
    preferred_source: ValidatorId,
    current_source: Option<ValidatorId>,
    attempted_sources: BTreeSet<ValidatorId>,
    total_len: Option<u64>,
    bytes: Vec<u8>,
}

impl PreparedTaskFetch {
    fn new(
        validator_set_version: u64,
        scope: ConsensusScope,
        expected_plan_digest: [u8; 32],
        announced_round: u64,
        preferred_source: ValidatorId,
    ) -> Self {
        Self {
            validator_set_version,
            scope,
            expected_plan_digest,
            announced_round,
            preferred_source,
            current_source: None,
            attempted_sources: BTreeSet::new(),
            total_len: None,
            bytes: Vec::new(),
        }
    }

    fn reset_source(&mut self) {
        self.current_source = None;
        self.total_len = None;
        self.bytes.clear();
    }

    fn matches(&self, validator_set_version: u64, expected_plan_digest: [u8; 32]) -> bool {
        self.validator_set_version == validator_set_version
            && self.expected_plan_digest == expected_plan_digest
    }
}

impl ValidatorBftRuntime {
    pub(crate) fn begin_prepared_task_fetch(
        &self,
        source: ValidatorId,
        validator_set_version: u64,
        scope: ConsensusScope,
        expected_plan_digest: [u8; 32],
        announced_round: u64,
    ) {
        let mut sync = self
            .inner
            .task_sync
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        if !sync.fetches.contains_key(&scope)
            && sync.fetches.len() >= MAX_PENDING_UNREGISTERED_SCOPES
        {
            return;
        }

        match sync.fetches.get_mut(&scope) {
            Some(existing)
                if existing.matches(validator_set_version, expected_plan_digest)
                    && announced_round <= existing.announced_round =>
            {
                if existing.current_source.is_none() {
                    existing.preferred_source = source;
                }
                return;
            }
            Some(existing) if announced_round <= existing.announced_round => return,
            _ => {}
        }

        sync.fetches.insert(
            scope.clone(),
            PreparedTaskFetch::new(
                validator_set_version,
                scope,
                expected_plan_digest,
                announced_round,
                source,
            ),
        );
    }

    pub(crate) fn has_pending_prepared_task_sync(&self) -> bool {
        let sync = self
            .inner
            .task_sync
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        !sync.fetches.is_empty() || !sync.announcements.is_empty()
    }

    pub(crate) fn retry_prepared_task_fetches(&self, reset_attempted: bool) {
        let scopes = {
            let sync = self
                .inner
                .task_sync
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            sync.fetches.keys().cloned().collect::<Vec<_>>()
        };

        for scope in scopes {
            self.request_next_prepared_task_source(&scope, reset_attempted);
        }
    }

    pub(crate) fn retry_prepared_task_sync(&self, reset_attempted: bool) {
        self.retry_prepared_task_fetches(reset_attempted);
        self.retry_prepared_task_announcements();
    }

    fn retry_prepared_task_announcements(&self) {
        let pending = {
            let sync = self
                .inner
                .task_sync
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            sync.announcements.values().cloned().collect::<Vec<_>>()
        };

        for announcement in pending {
            let message = BftNetworkMessage::PreparedTaskAvailable {
                validator_set_version: announcement.validator_set_version,
                scope: announcement.scope,
                expected_plan_digest: announcement.expected_plan_digest,
                round: announcement.round,
            };
            let _ = self.send_direct(announcement.proposer, &message);
        }
    }

    pub(crate) fn acknowledge_prepared_task_announcement(
        &self,
        proposer: ValidatorId,
        validator_set_version: u64,
        scope: &ConsensusScope,
        expected_plan_digest: [u8; 32],
    ) {
        let mut sync = self
            .inner
            .task_sync
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let should_remove = sync.announcements.get(scope).is_some_and(|announcement| {
            announcement.proposer == proposer
                && announcement.validator_set_version == validator_set_version
                && announcement.expected_plan_digest == expected_plan_digest
        });
        if should_remove {
            sync.announcements.remove(scope);
        }
    }

    fn request_next_prepared_task_source(&self, scope: &ConsensusScope, reset_attempted: bool) {
        let connected = self.connected_validator_ids();
        let request = {
            let mut sync = self
                .inner
                .task_sync
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(fetch) = sync.fetches.get_mut(scope) else {
                return;
            };
            if fetch.current_source.is_some() {
                return;
            }
            if reset_attempted {
                fetch.attempted_sources.clear();
            }

            let candidate = std::iter::once(fetch.preferred_source)
                .chain(connected)
                .find(|candidate| {
                    *candidate != self.validator_id()
                        && !fetch.attempted_sources.contains(candidate)
                });
            let Some(candidate) = candidate else {
                return;
            };
            fetch.attempted_sources.insert(candidate);
            fetch.current_source = Some(candidate);
            fetch.total_len = None;
            fetch.bytes.clear();
            (
                candidate,
                BftNetworkMessage::PreparedTaskRequest {
                    validator_set_version: fetch.validator_set_version,
                    scope: fetch.scope.clone(),
                    expected_plan_digest: fetch.expected_plan_digest,
                    offset: 0,
                },
            )
        };

        if self.send_direct(request.0, &request.1).is_err() {
            let mut sync = self
                .inner
                .task_sync
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(fetch) = sync.fetches.get_mut(scope)
                && fetch.current_source == Some(request.0)
            {
                fetch.reset_source();
            }
            drop(sync);
            self.request_next_prepared_task_source(scope, false);
        }
    }

    pub(crate) fn serve_prepared_task_request(
        &self,
        requester: ValidatorId,
        validator_set_version: u64,
        scope: ConsensusScope,
        expected_plan_digest: [u8; 32],
        offset: u64,
    ) {
        self.acknowledge_prepared_task_announcement(
            requester,
            validator_set_version,
            &scope,
            expected_plan_digest,
        );
        let unavailable = || BftNetworkMessage::PreparedTaskSourceUnavailable {
            validator_set_version,
            scope: scope.clone(),
            expected_plan_digest,
        };

        let ConsensusScope::PreparedTask(task_id) = &scope else {
            let _ = self.send_direct(requester, &unavailable());
            return;
        };

        let prepared = match self.inner.store.load_prepared_tasks() {
            Ok(tasks) => tasks.get(task_id).cloned(),
            Err(_) => None,
        };
        let Some(prepared) = prepared else {
            let _ = self.send_direct(requester, &unavailable());
            return;
        };
        let Ok(plan_digest) = prepared.plan_digest() else {
            let _ = self.send_direct(requester, &unavailable());
            return;
        };
        if prepared.validator_set_version != validator_set_version
            || plan_digest != expected_plan_digest
        {
            let _ = self.send_direct(requester, &unavailable());
            return;
        }

        let Ok(source) = encode_legal_task(&prepared.source_task) else {
            let _ = self.send_direct(requester, &unavailable());
            return;
        };
        if source.len() > MAX_PREPARED_TASK_SOURCE_SIZE {
            let _ = self.send_direct(requester, &unavailable());
            return;
        }
        let Ok(offset) = usize::try_from(offset) else {
            let _ = self.send_direct(requester, &unavailable());
            return;
        };
        if offset > source.len() {
            let _ = self.send_direct(requester, &unavailable());
            return;
        }

        let end = offset
            .saturating_add(MAX_PREPARED_TASK_SOURCE_CHUNK_SIZE)
            .min(source.len());
        let message = BftNetworkMessage::PreparedTaskSourceChunk {
            validator_set_version,
            scope,
            expected_plan_digest,
            total_len: source.len() as u64,
            offset: offset as u64,
            bytes: source[offset..end].to_vec(),
        };
        let _ = self.send_direct(requester, &message);
    }

    pub(crate) fn ingest_prepared_task_chunk(
        &self,
        chunk: PreparedTaskChunk,
    ) -> PreparedTaskChunkResult {
        let PreparedTaskChunk {
            sender,
            validator_set_version,
            scope,
            expected_plan_digest,
            total_len,
            offset,
            bytes,
        } = chunk;
        let Ok(total_len_usize) = usize::try_from(total_len) else {
            return PreparedTaskChunkResult::Reject;
        };
        if total_len_usize > MAX_PREPARED_TASK_SOURCE_SIZE || bytes.is_empty() {
            return PreparedTaskChunkResult::Reject;
        }

        let mut sync = self
            .inner
            .task_sync
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(fetch) = sync.fetches.get_mut(&scope) else {
            return PreparedTaskChunkResult::Reject;
        };
        if !fetch.matches(validator_set_version, expected_plan_digest)
            || fetch.current_source != Some(sender)
            || offset != fetch.bytes.len() as u64
            || bytes.len() > MAX_PREPARED_TASK_SOURCE_CHUNK_SIZE
            || total_len < offset.saturating_add(bytes.len() as u64)
            || fetch
                .total_len
                .is_some_and(|expected| expected != total_len)
        {
            return PreparedTaskChunkResult::Reject;
        }

        fetch.total_len = Some(total_len);
        fetch.bytes.extend_from_slice(&bytes);
        if fetch.bytes.len() as u64 == total_len {
            return PreparedTaskChunkResult::Complete(fetch.bytes.clone());
        }

        let request = BftNetworkMessage::PreparedTaskRequest {
            validator_set_version,
            scope: scope.clone(),
            expected_plan_digest,
            offset: fetch.bytes.len() as u64,
        };
        drop(sync);
        if self.send_direct(sender, &request).is_err() {
            self.reject_prepared_task_source(&scope, sender);
        }
        PreparedTaskChunkResult::Pending
    }

    pub(crate) fn reject_prepared_task_source(&self, scope: &ConsensusScope, sender: ValidatorId) {
        {
            let mut sync = self
                .inner
                .task_sync
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(fetch) = sync.fetches.get_mut(scope)
                && fetch.current_source == Some(sender)
            {
                fetch.reset_source();
            }
        }
        self.request_next_prepared_task_source(scope, false);
    }

    pub(crate) fn finish_prepared_task_fetch(&self, scope: &ConsensusScope) {
        self.inner
            .task_sync
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .fetches
            .remove(scope);
    }

    pub(crate) fn announce_prepared_task(
        &self,
        validator_set: &ValidatorSet,
        scope: ConsensusScope,
        expected_plan_digest: [u8; 32],
    ) {
        let round = self
            .inner
            .store
            .bft_local_state(self.validator_id(), &scope)
            .ok()
            .flatten()
            .map(|state| state.round())
            .unwrap_or(0);
        let proposer = validator_set.proposer(round);
        if proposer == self.validator_id() {
            return;
        }

        let announcement = PreparedTaskAnnouncement {
            validator_set_version: validator_set.version(),
            scope: scope.clone(),
            expected_plan_digest,
            round,
            proposer,
        };
        self.inner
            .task_sync
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .announcements
            .insert(scope, announcement);
        self.retry_prepared_task_announcements();
    }

    fn send_direct(
        &self,
        validator_id: ValidatorId,
        message: &BftNetworkMessage,
    ) -> Result<(), NetworkError> {
        let mut outbound = self
            .inner
            .outbound
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        super::prune_dead_outbound(&mut outbound);
        let Some(managed) = outbound.get(&validator_id) else {
            return Err(NetworkError::Transport(
                "validator BFT peer is not connected".to_owned(),
            ));
        };
        if !managed.peer.accepts_recipient(message) {
            return Err(NetworkError::BftUnauthorized);
        }
        let sender = if matches!(
            message,
            BftNetworkMessage::FinalityVote { .. } | BftNetworkMessage::FinalityCertificate { .. }
        ) {
            &managed.finality_sender
        } else {
            &managed.sender
        };
        if let Err(error) = sender.try_send(message.clone()) {
            let detail = match error {
                tokio::sync::mpsc::error::TrySendError::Full(_) => {
                    "validator BFT send queue is full"
                }
                tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                    "validator BFT send worker is closed"
                }
            };
            managed
                .alive
                .store(false, std::sync::atomic::Ordering::Release);
            if let Some(managed) = outbound.remove(&validator_id) {
                managed.peer.close();
            }
            return Err(NetworkError::Transport(detail.to_owned()));
        }
        Ok(())
    }
}
