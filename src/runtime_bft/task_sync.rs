use std::collections::{BTreeMap, BTreeSet};

use crate::legal_task_codec::encode_legal_task;
use crate::network::MAX_PREPARED_TASK_SOURCE_CHUNK_SIZE;
use crate::runtime_bft::ValidatorBftRuntime;
use crate::runtime_bft_consensus::MAX_PENDING_UNREGISTERED_SCOPES;
use crate::{BftNetworkMessage, ConsensusScope, NetworkError, ValidatorId, ValidatorSet};

#[cfg(test)]
mod tests;

pub(crate) const MAX_PREPARED_TASK_SOURCE_SIZE: usize =
    crate::prepared::source::MAX_PREPARED_SOURCE_SIZE;

pub(crate) fn superseded_allocation_scope(
    scope: &ConsensusScope,
    current_version: u64,
    frontier: u64,
) -> bool {
    matches!(scope, ConsensusScope::CurrencyAllocation { validator_set_version, start }
        if *validator_set_version < current_version
            || (*validator_set_version == current_version && *start < frontier))
}

#[derive(Default)]
pub(super) struct PreparedTaskSyncState {
    fetches: BTreeMap<(ConsensusScope, [u8; 32]), PreparedTaskFetch>,
    announcements: BTreeMap<(ConsensusScope, [u8; 32]), PreparedTaskAnnouncement>,
    collected_transitions: BTreeSet<(ConsensusScope, [u8; 32])>,
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
    Ignored,
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
    deadline: Option<tokio::time::Instant>,
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
            deadline: None,
        }
    }

    fn reset_source(&mut self) {
        self.current_source = None;
        self.total_len = None;
        self.bytes.clear();
        self.deadline = None;
    }

    fn matches(&self, validator_set_version: u64, expected_plan_digest: [u8; 32]) -> bool {
        self.validator_set_version == validator_set_version
            && self.expected_plan_digest == expected_plan_digest
    }

    fn select_source(
        &mut self,
        connected: &[ValidatorId],
        local_validator: ValidatorId,
        reset_attempted: bool,
        now: tokio::time::Instant,
        timeout: std::time::Duration,
    ) -> Option<ValidatorId> {
        if let Some(source) = self.current_source {
            if connected.contains(&source) && self.deadline.is_some_and(|deadline| deadline > now) {
                return None;
            }
            self.reset_source();
        }

        let eligible = std::iter::once(self.preferred_source)
            .chain(connected.iter().copied())
            .filter(|candidate| *candidate != local_validator && connected.contains(candidate))
            .collect::<Vec<_>>();
        let mut candidate = eligible
            .iter()
            .copied()
            .find(|candidate| !self.attempted_sources.contains(candidate));
        if candidate.is_none() && reset_attempted {
            self.attempted_sources.clear();
            candidate = eligible.first().copied();
        }
        let candidate = candidate?;

        self.attempted_sources.insert(candidate);
        self.current_source = Some(candidate);
        self.total_len = None;
        self.bytes.clear();
        self.deadline = now.checked_add(timeout);
        Some(candidate)
    }
}

impl ValidatorBftRuntime {
    pub(crate) fn retire_superseded_allocation_sync(&self, current_version: u64, frontier: u64) {
        let mut sync = self
            .inner
            .task_sync
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        sync.fetches
            .retain(|(scope, _), _| !superseded_allocation_scope(scope, current_version, frontier));
        sync.announcements
            .retain(|(scope, _), _| !superseded_allocation_scope(scope, current_version, frontier));
        sync.collected_transitions
            .retain(|(scope, _)| !superseded_allocation_scope(scope, current_version, frontier));
    }

    pub(crate) fn has_collected_transition(
        &self,
        scope: &ConsensusScope,
        digest: [u8; 32],
    ) -> bool {
        self.inner
            .task_sync
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .collected_transitions
            .contains(&(scope.clone(), digest))
    }

    pub(crate) fn remember_collected_transition(&self, scope: &ConsensusScope, digest: [u8; 32]) {
        let mut sync = self
            .inner
            .task_sync
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // This only suppresses repeated collection pulls; it never admits a voting candidate.
        if sync.collected_transitions.len() < MAX_PENDING_UNREGISTERED_SCOPES {
            sync.collected_transitions.insert((scope.clone(), digest));
        }
    }

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

        let key = (scope.clone(), expected_plan_digest);
        if !sync.fetches.contains_key(&key) && sync.fetches.len() >= MAX_PENDING_UNREGISTERED_SCOPES
        {
            return;
        }

        match sync.fetches.get_mut(&key) {
            Some(existing) if existing.matches(validator_set_version, expected_plan_digest) => {
                existing.announced_round = existing.announced_round.max(announced_round);
                if existing.current_source.is_none() {
                    existing.preferred_source = source;
                }
                return;
            }
            _ => {}
        }

        sync.fetches.insert(
            key,
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

        for (scope, digest) in scopes {
            self.request_next_prepared_task_source(&scope, digest, reset_attempted);
        }
    }

    pub(crate) fn retry_prepared_task_sync(&self, reset_attempted: bool) {
        self.retry_prepared_task_fetches(reset_attempted);
        self.retry_prepared_task_announcements();
    }

    fn retry_prepared_task_announcements(&self) {
        let Some(snapshot) = self.inner.store.load_shared().ok().flatten() else {
            return;
        };
        let pending = {
            let mut sync = self
                .inner
                .task_sync
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            sync.announcements
                .values_mut()
                .filter_map(|announcement| {
                    let validators = (snapshot.validator_set.version()
                        == announcement.validator_set_version)
                        .then_some(&snapshot.validator_set)
                        .or_else(|| {
                            snapshot
                                .retained_validator_sets
                                .get(&announcement.validator_set_version)
                        })?;
                    let round = snapshot
                        .bft_local_states
                        .get(&(self.validator_id(), announcement.scope.clone()))
                        .filter(|state| {
                            state.validator_set_version() == announcement.validator_set_version
                        })
                        .map_or(announcement.round, |state| state.round());
                    announcement.round = announcement.round.max(round);
                    announcement.proposer = validators.proposer(announcement.round);
                    (announcement.proposer != self.validator_id()).then(|| announcement.clone())
                })
                .collect::<Vec<_>>()
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
        let key = (scope.clone(), expected_plan_digest);
        let should_remove = sync.announcements.get(&key).is_some_and(|announcement| {
            announcement.proposer == proposer
                && announcement.validator_set_version == validator_set_version
                && announcement.expected_plan_digest == expected_plan_digest
        });
        if should_remove {
            sync.announcements.remove(&key);
        }
    }

    fn request_next_prepared_task_source(
        &self,
        scope: &ConsensusScope,
        digest: [u8; 32],
        reset_attempted: bool,
    ) {
        let connected = self.connected_validator_ids();
        let request = {
            let mut sync = self
                .inner
                .task_sync
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(fetch) = sync.fetches.get_mut(&(scope.clone(), digest)) else {
                return;
            };
            let Some(candidate) = fetch.select_source(
                &connected,
                self.validator_id(),
                reset_attempted,
                tokio::time::Instant::now(),
                self.bft_timeouts().proposal,
            ) else {
                return;
            };
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
            if let Some(fetch) = sync.fetches.get_mut(&(scope.clone(), digest))
                && fetch.current_source == Some(request.0)
            {
                fetch.reset_source();
            }
            drop(sync);
            self.request_next_prepared_task_source(scope, digest, false);
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
        let unavailable = || BftNetworkMessage::PreparedTaskSourceUnavailable {
            validator_set_version,
            scope: scope.clone(),
            expected_plan_digest,
        };

        if let Some(snapshot) = self.inner.store.load_shared().ok().flatten()
            && let Some((total_len, bytes)) =
                crate::runtime_governance::handoff_source::source_chunk(
                    &snapshot,
                    &scope,
                    expected_plan_digest,
                    offset,
                )
        {
            let _ = self.send_direct(
                requester,
                &BftNetworkMessage::PreparedTaskSourceChunk {
                    validator_set_version,
                    scope,
                    expected_plan_digest,
                    total_len,
                    offset,
                    bytes,
                },
            );
            return;
        }

        if let Some(bytes) = self
            .consensus()
            .allocation_transition_source(&scope, expected_plan_digest)
        {
            let _ = self.send_direct(
                requester,
                &BftNetworkMessage::ValidatorSetTransitionSource {
                    collecting: false,
                    validator_set_version,
                    scope,
                    bytes,
                },
            );
            return;
        }
        let mut proofs = Vec::new();
        let source = match &scope {
            ConsensusScope::CurrencyAllocation {
                validator_set_version: version,
                start,
            } => {
                let task = self
                    .consensus()
                    .allocation_source(&scope, expected_plan_digest)
                    .or_else(|| {
                        let snapshot = self.inner.store.load_shared().ok()??;
                        snapshot
                            .state
                            .protocol
                            .task_bindings
                            .iter()
                            .find_map(|(task_id, binding)| {
                                let certificate = binding.allocation_certificate.as_ref()?;
                                if binding.allocation?.0 != *start
                                    || certificate.statement().validator_set_version() != *version
                                    || certificate.statement().subject_digest()
                                        != expected_plan_digest
                                {
                                    return None;
                                }
                                binding.allocation_task.clone().or_else(|| {
                                    snapshot
                                        .prepared_tasks
                                        .get(task_id)
                                        .map(|prepared| prepared.source_task.clone())
                                })
                            })
                            .or_else(|| {
                                snapshot.task_receipts.values().find_map(|receipt| {
                                    let (bound_start, certificate) = receipt.allocation()?;
                                    (*bound_start == *start
                                        && certificate.statement().validator_set_version()
                                            == *version
                                        && certificate.statement().subject_digest()
                                            == expected_plan_digest)
                                        .then(|| receipt.plan().source_task.clone())
                                })
                            })
                    });
                task.and_then(|task| encode_legal_task(&task).ok().map(std::sync::Arc::new))
            }
            ConsensusScope::PreparedTask(task_id) => (|| {
                let snapshot = self.inner.store.load_shared().ok()??;
                if let Some(receipt) = snapshot.task_receipts.get(task_id) {
                    if receipt.plan().validator_set_version != validator_set_version
                        || receipt.plan().plan_digest().ok()? != expected_plan_digest
                    {
                        return None;
                    }
                    if offset == 0 {
                        proofs.push(BftNetworkMessage::FinalityCertificate {
                            scope: scope.clone(),
                            certificate: receipt.certificate().ok()?,
                        });
                        if let Some((start, certificate)) = receipt.allocation() {
                            proofs.push(BftNetworkMessage::FinalityCertificate {
                                scope: ConsensusScope::CurrencyAllocation {
                                    validator_set_version: certificate
                                        .statement()
                                        .validator_set_version(),
                                    start: *start,
                                },
                                certificate: certificate.clone(),
                            });
                        }
                    }
                    return receipt.source().ok();
                }
                let prepared = snapshot.prepared_tasks.get(task_id)?;
                if prepared.validator_set_version != validator_set_version {
                    return None;
                }
                let candidate = prepared.candidate(expected_plan_digest).ok()?.or_else(|| {
                    self.inner
                        .store
                        .prepared_abort_statement(task_id)
                        .ok()
                        .filter(|statement| statement.subject_digest() == expected_plan_digest)
                        .map(|_| prepared.clone())
                })?;
                if offset == 0
                    && prepared.plan_digest().ok()? == expected_plan_digest
                    && let Some(certificate) = prepared.finality_certificate().ok()?
                {
                    proofs.push(BftNetworkMessage::FinalityCertificate {
                        scope: scope.clone(),
                        certificate,
                    });
                }
                if offset == 0
                    && let Some(binding) = snapshot.state.protocol.task_bindings.get(task_id)
                    && let (Some((start, _)), Some(certificate)) =
                        (binding.allocation, &binding.allocation_certificate)
                {
                    proofs.push(BftNetworkMessage::FinalityCertificate {
                        scope: ConsensusScope::CurrencyAllocation {
                            validator_set_version: certificate.statement().validator_set_version(),
                            start,
                        },
                        certificate: certificate.clone(),
                    });
                }
                candidate.encode_source().ok().map(std::sync::Arc::new)
            })(),
            _ => None,
        };
        let Some(source) = source else {
            let _ = self.send_direct(requester, &unavailable());
            return;
        };
        for proof in proofs {
            let _ = self.send_direct(requester, &proof);
        }
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
        let mut sync = self
            .inner
            .task_sync
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let buffered = sync
            .fetches
            .values()
            .map(|fetch| fetch.bytes.len())
            .sum::<usize>();
        let Some(fetch) = sync.fetches.get_mut(&(scope.clone(), expected_plan_digest)) else {
            return PreparedTaskChunkResult::Ignored;
        };
        if !fetch.matches(validator_set_version, expected_plan_digest)
            || fetch.current_source != Some(sender)
        {
            return PreparedTaskChunkResult::Ignored;
        }
        let Ok(total_len_usize) = usize::try_from(total_len) else {
            return PreparedTaskChunkResult::Reject;
        };
        let size_limit = if matches!(scope, ConsensusScope::CurrencyAllocation { .. }) {
            crate::runtime_governance::handoff_source::MAX_SOURCE_SIZE
        } else {
            MAX_PREPARED_TASK_SOURCE_SIZE
        };
        if total_len_usize > size_limit
            || (total_len_usize > MAX_PREPARED_TASK_SOURCE_SIZE
                && !crate::runtime_governance::handoff_source::is_handoff_source(if offset == 0 {
                    &bytes
                } else {
                    &fetch.bytes
                }))
            || bytes.is_empty()
            || bytes.len() > MAX_PREPARED_TASK_SOURCE_CHUNK_SIZE
            || total_len < offset.saturating_add(bytes.len() as u64)
            || fetch
                .total_len
                .is_some_and(|expected| expected != total_len)
        {
            return PreparedTaskChunkResult::Reject;
        }
        if offset < fetch.bytes.len() as u64 {
            let start = offset as usize;
            return if fetch.bytes.get(start..start.saturating_add(bytes.len()))
                == Some(bytes.as_slice())
            {
                PreparedTaskChunkResult::Ignored
            } else {
                PreparedTaskChunkResult::Reject
            };
        }
        if offset != fetch.bytes.len() as u64 {
            return PreparedTaskChunkResult::Reject;
        }

        let budget = crate::runtime_governance::handoff_source::MAX_SOURCE_SIZE
            + MAX_PREPARED_TASK_SOURCE_SIZE * MAX_PENDING_UNREGISTERED_SCOPES;
        if buffered.saturating_add(bytes.len()) > budget {
            return PreparedTaskChunkResult::Reject;
        }

        fetch.total_len = Some(total_len);
        fetch.bytes.extend_from_slice(&bytes);
        fetch.deadline = tokio::time::Instant::now().checked_add(self.bft_timeouts().proposal);
        if fetch.bytes.len() as u64 == total_len {
            return PreparedTaskChunkResult::Complete(std::mem::take(&mut fetch.bytes));
        }

        let request = BftNetworkMessage::PreparedTaskRequest {
            validator_set_version,
            scope: scope.clone(),
            expected_plan_digest,
            offset: fetch.bytes.len() as u64,
        };
        drop(sync);
        if self.send_direct(sender, &request).is_err() {
            self.reject_prepared_task_source(
                &scope,
                sender,
                validator_set_version,
                expected_plan_digest,
            );
        }
        PreparedTaskChunkResult::Pending
    }

    pub(crate) fn reject_prepared_task_source(
        &self,
        scope: &ConsensusScope,
        sender: ValidatorId,
        validator_set_version: u64,
        expected_plan_digest: [u8; 32],
    ) {
        {
            let mut sync = self
                .inner
                .task_sync
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(fetch) = sync.fetches.get_mut(&(scope.clone(), expected_plan_digest))
                && fetch.current_source == Some(sender)
                && fetch.matches(validator_set_version, expected_plan_digest)
            {
                fetch.reset_source();
            } else {
                return;
            }
        }
        self.request_next_prepared_task_source(scope, expected_plan_digest, false);
    }

    pub(crate) fn finish_prepared_task_fetch(&self, scope: &ConsensusScope, digest: [u8; 32]) {
        self.inner
            .task_sync
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .fetches
            .remove(&(scope.clone(), digest));
    }

    pub(crate) fn finish_prepared_task_sync(&self, scope: &ConsensusScope) {
        let mut sync = self
            .inner
            .task_sync
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        sync.fetches
            .retain(|(candidate_scope, _), _| candidate_scope != scope);
        sync.announcements
            .retain(|(candidate_scope, _), _| candidate_scope != scope);
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
            .insert((scope.clone(), expected_plan_digest), announcement);
        if proposer != self.validator_id() {
            let _ = self.send_direct(
                proposer,
                &BftNetworkMessage::PreparedTaskAvailable {
                    validator_set_version: validator_set.version(),
                    scope,
                    expected_plan_digest,
                    round,
                },
            );
        }
    }

    pub(super) fn send_direct(
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
        if !managed.peer.can_send(message) {
            return Err(NetworkError::BftUnauthorized);
        }
        if let Err(error) = managed.enqueue(message) {
            if !managed.alive.load(std::sync::atomic::Ordering::Acquire)
                && let Some(managed) = outbound.remove(&validator_id)
            {
                managed.peer.close();
            }
            return Err(error);
        }
        Ok(())
    }
}
