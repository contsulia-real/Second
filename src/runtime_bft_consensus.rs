use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::Notify;
use tokio::time::Instant;

mod allocation;
mod candidates;
mod completion;
mod finality;
mod governance;
mod public_checkpoint;
mod runner;

use finality::{
    begin_business_finality, ingest_finality_certificate, ingest_finality_vote,
    rebroadcast_finality_votes, remember_finality_qc, schedule_finality_relay,
};

use crate::runtime_bft::{InboundBftMessage, ValidatorBftRuntime};
use crate::runtime_consensus_target::{
    CertifiedConsensusTarget, ConsensusTargetError, ValidatorConsensusTarget,
};
use crate::{
    AuthorizationError, BftDriver, BftDriverAction, BftDriverError, BftDriverPhase,
    BftNetworkMessage, BftPhase, BftProposalSubject, BftQuorumCertificate, BftTimeoutConfig,
    BftValue, CertifiedPublicCurrencyCheckpoint, CertifiedStateRecoveryCheckpoint,
    CertifiedValidatorSetTransition, ConsensusScope, FinalityCertificate, FinalityError,
    NodeRuntime, NodeRuntimeError, PersistenceError, PreparationError, PublicCurrencyCheckpoint,
    StateRecoveryCheckpoint, TaskId, ValidatorBftSendFailure, ValidatorId, ValidatorSet,
    ValidatorSetTransition, ValidatorSetTransitionSource, ValidatorSigner, ValidatorSigningError,
    ValidatorTransitionError, ValidatorTransitionSourceCodecError, ValidatorVote,
};

pub(crate) struct ValidatorConsensusRuntime {
    coordinator: Mutex<BftConsensusCoordinator>,
    activity: Notify,
}

impl ValidatorConsensusRuntime {
    pub(crate) fn new() -> Self {
        Self {
            coordinator: Mutex::new(BftConsensusCoordinator::new()),
            activity: Notify::new(),
        }
    }

    pub(crate) fn register(
        &self,
        signer: ValidatorSigner,
        store: crate::StateStore,
        validator_set: ValidatorSet,
        target: ValidatorConsensusTarget,
        timeouts: BftTimeoutConfig,
    ) -> Result<(), BftConsensusRuntimeError> {
        self.coordinator
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .register(signer, store, validator_set, target, timeouts)?;
        self.activity.notify_one();
        Ok(())
    }

    pub(crate) fn drain_events(&self) -> Vec<BftConsensusEvent> {
        self.coordinator
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .drain_events()
    }

    pub(crate) fn completed_relay_authorities(&self) -> Vec<(ConsensusScope, ValidatorSet)> {
        self.coordinator
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .completed_relay_authorities()
    }

    pub(crate) fn drive(
        &self,
        inbound: Vec<InboundBftMessage>,
        now: Instant,
    ) -> BftConsensusOutput {
        self.coordinator
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .drive(inbound, now)
    }

    pub(crate) fn record_send_failures(
        &self,
        scope: ConsensusScope,
        failures: Vec<ValidatorBftSendFailure>,
    ) {
        self.coordinator
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .record_send_failures(scope, failures);
    }

    pub(crate) fn record_rejection(
        &self,
        validator_id: Option<ValidatorId>,
        scope: ConsensusScope,
        error: BftConsensusRuntimeError,
    ) {
        self.coordinator
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .record_rejection(validator_id, scope, error);
    }

    pub(crate) fn record_connection_failure(
        &self,
        node_id: crate::NodeId,
        address: std::net::SocketAddr,
        elapsed: Duration,
        error: crate::ValidatorBftRuntimeError,
    ) {
        self.coordinator
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .record_event(BftConsensusEvent::ConnectionFailed {
                node_id,
                address,
                elapsed,
                error,
            });
    }

    pub(crate) fn wake(&self) {
        self.activity.notify_one();
    }

    fn next_deadline(&self) -> Option<Instant> {
        self.coordinator
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .next_deadline()
    }

    pub(crate) async fn wait_for_activity_or_deadline(
        &self,
        deadline: Option<Instant>,
        sync_retry: Option<Instant>,
    ) -> bool {
        if sync_retry.is_some_and(|retry| retry <= Instant::now()) {
            return true;
        }
        let deadline = match (deadline, sync_retry) {
            (Some(consensus), Some(sync)) => Some(consensus.min(sync)),
            (deadline, None) | (None, deadline) => deadline,
        };
        match deadline {
            Some(deadline) => {
                tokio::select! {
                    biased;
                    _ = tokio::time::sleep_until(deadline) => {
                        sync_retry.is_some_and(|retry| retry <= Instant::now())
                    }
                    _ = self.activity.notified() => false,
                }
            }
            None => {
                self.activity.notified().await;
                false
            }
        }
    }
}

pub(crate) fn start_prepared_task_consensus_for(
    store: &crate::StateStore,
    runtime: &ValidatorBftRuntime,
    task_id: TaskId,
) -> Result<(), NodeRuntimeError> {
    let snapshot = store.load_shared()?;
    if snapshot.as_ref().is_some_and(|snapshot| {
        snapshot.prepared_tasks.get(&task_id).is_some_and(|plan| {
            snapshot
                .retained_validator_sets
                .get(&plan.validator_set_version)
                .is_some_and(|origin| !origin.contains(runtime.validator_id()))
                && snapshot
                    .state
                    .protocol
                    .task_handoff
                    .as_ref()
                    .and_then(|handoff| handoff.task_context(&task_id))
                    .is_some_and(|inherited| {
                        inherited.validator_set_version == plan.validator_set_version
                            && inherited.request_digest == plan.request_digest
                            && inherited.source_task == plan.source_task
                    })
        })
    }) {
        // New members can apply inherited terminal proofs, but cannot create
        // an old-committee voting session even if the body is locally prepared.
        return Ok(());
    }
    if snapshot.as_ref().is_some_and(|snapshot| {
        snapshot.prepared_tasks.get(&task_id).is_some_and(|plan| {
            if plan.phase == crate::prepared_plan::PreparedTaskPhase::Finalized {
                return true;
            }
            if plan.conflict_abort {
                return false;
            }
            if !plan.commit_authorized {
                return true;
            }
            snapshot
                .bft_local_states
                .get(&(
                    runtime.validator_id(),
                    ConsensusScope::PreparedTask(task_id.clone()),
                ))
                .and_then(|local| local.valid_prevote_qc())
                .and_then(|qc| qc.statement().value().digest())
                .is_some_and(|digest| {
                    plan.candidate(digest)
                        .ok()
                        .flatten()
                        .is_some_and(|candidate| !candidate.commit_authorized)
                })
        })
    }) {
        return Ok(());
    }
    let additional_candidates = snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.prepared_tasks.get(&task_id))
        .filter(|plan| !plan.variants.is_empty())
        .map(|plan| plan.owned_candidate_digests())
        .transpose()
        .map_err(|_| crate::PersistenceError::InvalidSnapshot)?
        .unwrap_or_default();
    let target =
        ValidatorConsensusTarget::prepared_task(store, runtime.validator_id(), task_id.clone())
            .map_err(BftConsensusRuntimeError::from)?;
    let ValidatorConsensusTarget::PreparedTask {
        plan_digest: initial_digest,
        ..
    } = &target
    else {
        unreachable!();
    };
    let initial_digest = *initial_digest;
    start_validator_consensus_target_for(store, runtime, target)?;
    for plan_digest in additional_candidates {
        if plan_digest == initial_digest {
            continue;
        }
        start_validator_consensus_target_for(
            store,
            runtime,
            ValidatorConsensusTarget::PreparedTask {
                task_id: task_id.clone(),
                plan_digest,
            },
        )?;
    }
    Ok(())
}

pub(crate) fn start_validator_consensus_target_for(
    store: &crate::StateStore,
    runtime: &ValidatorBftRuntime,
    target: ValidatorConsensusTarget,
) -> Result<(), NodeRuntimeError> {
    let target = match target {
        ValidatorConsensusTarget::ValidatorSetTransition(transition) => {
            ValidatorConsensusTarget::ValidatorSetTransition(
                store.prepare_validator_set_transition(transition)?,
            )
        }
        value => value,
    };
    runtime.refresh_authority()?;
    let active_validator_set = runtime.validator_set();
    let validator_set = target
        .validator_set(store, &active_validator_set)
        .map_err(BftConsensusRuntimeError::from)?;
    let signer = runtime.signer_for(&validator_set)?;
    let prepared_subject = matches!(target, ValidatorConsensusTarget::PreparedTask { .. })
        .then(|| target.proposal_subject(store))
        .transpose()
        .map_err(BftConsensusRuntimeError::from)?;
    let allocation_subject = matches!(target, ValidatorConsensusTarget::CurrencyAllocation(_))
        .then(|| target.proposal_subject(store))
        .transpose()
        .map_err(BftConsensusRuntimeError::from)?;
    store.admit_governance(&target)?;
    runtime.consensus().register(
        signer,
        store.clone(),
        validator_set.clone(),
        target,
        runtime.bft_timeouts(),
    )?;
    if let Some(subject) = prepared_subject
        && let ConsensusScope::PreparedTask(task_id) = subject.scope()
        && store.load_shared()?.is_some_and(|snapshot| {
            snapshot.prepared_tasks.get(task_id).is_some_and(|plan| {
                plan.candidate(subject.digest()).is_ok_and(|candidate| {
                    candidate.is_some_and(|candidate| candidate.commit_authorized)
                })
            })
        })
    {
        runtime.announce_prepared_task(&validator_set, subject.scope().clone(), subject.digest());
    }
    if let Some(subject) = allocation_subject {
        runtime.broadcast(&BftNetworkMessage::PreparedTaskAvailable {
            validator_set_version: validator_set.version(),
            scope: subject.scope().clone(),
            expected_plan_digest: subject.digest(),
            round: 0,
        });
    }
    Ok(())
}

impl NodeRuntime {
    pub fn start_prepared_task_consensus(&self, task_id: TaskId) -> Result<(), NodeRuntimeError> {
        let runtime = self
            .validator_bft
            .as_ref()
            .ok_or(NodeRuntimeError::ValidatorBftNotConfigured)?;
        start_prepared_task_consensus_for(self.full_store()?, runtime, task_id)
    }

    pub fn start_public_checkpoint_consensus(
        &self,
        checkpoint: PublicCurrencyCheckpoint,
    ) -> Result<(), NodeRuntimeError> {
        self.start_validator_consensus_target(ValidatorConsensusTarget::PublicCheckpoint(
            checkpoint,
        ))
    }

    pub fn start_validator_set_transition_consensus(
        &self,
        transition: ValidatorSetTransition,
    ) -> Result<(), NodeRuntimeError> {
        let context = self
            .governance_context()
            .ok_or(NodeRuntimeError::ValidatorBftNotConfigured)?;
        let transition = self
            .full_store()?
            .prepare_validator_set_transition(transition)?;
        context.begin_transition(&transition).map(|_| ())
    }

    pub fn start_state_recovery_checkpoint_consensus(
        &self,
        checkpoint: StateRecoveryCheckpoint,
    ) -> Result<(), NodeRuntimeError> {
        self.start_validator_consensus_target(ValidatorConsensusTarget::StateRecoveryCheckpoint(
            checkpoint,
        ))
    }

    fn start_validator_consensus_target(
        &self,
        target: ValidatorConsensusTarget,
    ) -> Result<(), NodeRuntimeError> {
        let runtime = self
            .validator_bft
            .as_ref()
            .ok_or(NodeRuntimeError::ValidatorBftNotConfigured)?;
        start_validator_consensus_target_for(self.full_store()?, runtime, target)
    }

    pub fn drain_bft_consensus_events(&self) -> Result<Vec<BftConsensusEvent>, NodeRuntimeError> {
        let runtime = self
            .validator_bft
            .as_ref()
            .ok_or(NodeRuntimeError::ValidatorBftNotConfigured)?;
        Ok(runtime.consensus().drain_events())
    }

    pub(crate) fn recover_validator_safety_if_ready(
        &self,
        runtime: &ValidatorBftRuntime,
    ) -> Result<(), NodeRuntimeError> {
        let store = self.full_store()?;
        let snapshot = store
            .load_shared()?
            .ok_or(PersistenceError::MissingSnapshot)?;
        if snapshot.validator_safety_ready
            || snapshot.pending_validator_safety_recovery.is_none()
            || snapshot.recovery_checkpoint_proof.is_none()
        {
            return Ok(());
        }

        let signer = runtime.signer_for(&snapshot.validator_set)?;
        store.try_complete_pending_validator_safety_recovery(
            signer.validator_id(),
            signer.consensus_public_key(),
        )?;
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BftConsensusRuntimeError {
    Driver(BftDriverError),
    InvalidGovernanceSource,
    GovernanceSourceCodec(ValidatorTransitionSourceCodecError),
    Persistence(PersistenceError),
    Authorization(AuthorizationError),
    Preparation(PreparationError),
    InvalidPreparedTaskSource,
    Signing(ValidatorSigningError),
    Finality(FinalityError),
    ValidatorTransition(ValidatorTransitionError),
    FinalityStatementMismatch,
    SubjectConflict(ConsensusScope),
    PendingFutureMessagesFull(ConsensusScope),
}

impl From<BftDriverError> for BftConsensusRuntimeError {
    fn from(value: BftDriverError) -> Self {
        Self::Driver(value)
    }
}

impl From<ConsensusTargetError> for BftConsensusRuntimeError {
    fn from(value: ConsensusTargetError) -> Self {
        match value {
            ConsensusTargetError::Persistence(error) => Self::Persistence(error),
            ConsensusTargetError::Signing(error) => Self::Signing(error),
            ConsensusTargetError::Finality(error) => Self::Finality(error),
            ConsensusTargetError::ValidatorTransition(error) => Self::ValidatorTransition(error),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BftConsensusEvent {
    ConnectionFailed {
        node_id: crate::NodeId,
        address: std::net::SocketAddr,
        elapsed: Duration,
        error: crate::ValidatorBftRuntimeError,
    },
    CertifiedCurrencyAllocation {
        allocation: crate::CurrencyAllocation,
        certificate: FinalityCertificate,
    },
    CertifiedPreparedTask {
        task_id: TaskId,
        certificate: FinalityCertificate,
    },
    CertifiedPublicCheckpoint(CertifiedPublicCurrencyCheckpoint),
    CertifiedValidatorSetTransition(CertifiedValidatorSetTransition),
    CertifiedStateRecoveryCheckpoint(CertifiedStateRecoveryCheckpoint),
    Rejected {
        validator_id: Option<ValidatorId>,
        scope: ConsensusScope,
        error: BftConsensusRuntimeError,
    },
    UnregisteredScope {
        validator_id: ValidatorId,
        scope: ConsensusScope,
    },
    SendFailed {
        scope: ConsensusScope,
        failures: Vec<ValidatorBftSendFailure>,
    },
}

pub(crate) const MAX_PENDING_UNREGISTERED_SCOPES: usize = 32;
pub(crate) const MAX_PENDING_MESSAGES_PER_SCOPE: usize = 64;
pub(crate) const MAX_BFT_RUNTIME_INBOUND_QUEUE: usize =
    MAX_PENDING_UNREGISTERED_SCOPES * MAX_PENDING_MESSAGES_PER_SCOPE;
const MAX_RECENT_COMPLETED_SCOPES: usize = 64;

pub(crate) struct BftConsensusCoordinator {
    sessions: BTreeMap<ConsensusScope, BftConsensusSession>,
    pending_unregistered: BTreeMap<ConsensusScope, VecDeque<InboundBftMessage>>,
    recent_completed: VecDeque<CompletedConsensusScope>,
    events: VecDeque<BftConsensusEvent>,
}

struct CompletedConsensusScope {
    recovery_source: Option<BftNetworkMessage>,
    allocation_source: Option<crate::LegalTask>,
    transition_source: Option<Vec<u8>>,
    subject: BftProposalSubject,
    validator_set: ValidatorSet,
    certificate: FinalityCertificate,
    relay_interval: Duration,
    next_relay_at: Instant,
}

struct BftConsensusSession {
    driver: BftDriver,
    store: crate::StateStore,
    signer: ValidatorSigner,
    validator_set: ValidatorSet,
    target: ValidatorConsensusTarget,
    candidates: BTreeMap<[u8; 32], ValidatorConsensusTarget>,
    valid_prevote_qc: Option<BftQuorumCertificate>,
    subject: BftProposalSubject,
    finality_votes: BTreeMap<ValidatorId, ValidatorVote>,
    finality_qc: Option<BftQuorumCertificate>,
    finality_certificate: Option<FinalityCertificate>,
    relayed_finality_certificate: bool,
    bft_finality_ready: bool,
    certified_emitted: bool,
    timeouts: BftTimeoutConfig,
    deadline: Option<Instant>,
    relayed_certificates: BTreeSet<(u64, BftPhase, BftValue)>,
    pending_future: VecDeque<InboundBftMessage>,
    needs_start: bool,
    // Work cursor only: restart repeats collection from the durable BFT round.
    collection_started_at_round: Option<u64>,
    finished: bool,
}

#[derive(Default)]
pub(crate) struct BftConsensusOutput {
    pub(crate) retry_prepared_tasks: BTreeSet<TaskId>,
    pub(crate) completed_prepared_tasks: Vec<TaskId>,
    pub(crate) currency_allocation_committed: bool,
    pub(crate) business_state_changed: bool,
    pub(crate) outbound: Vec<BftNetworkMessage>,
    pub(crate) certified_recovery_checkpoint: Option<CertifiedStateRecoveryCheckpoint>,
    pub(crate) validator_set_changed: bool,
}

impl BftConsensusCoordinator {
    pub(crate) fn new() -> Self {
        Self {
            sessions: BTreeMap::new(),
            pending_unregistered: BTreeMap::new(),
            recent_completed: VecDeque::new(),
            events: VecDeque::new(),
        }
    }

    pub(crate) fn completed_relay_authorities(&self) -> Vec<(ConsensusScope, ValidatorSet)> {
        self.recent_completed
            .iter()
            .map(|completed| {
                (
                    completed.subject.scope().clone(),
                    completed.validator_set.clone(),
                )
            })
            .collect()
    }

    pub(crate) fn register(
        &mut self,
        signer: ValidatorSigner,
        store: crate::StateStore,
        validator_set: ValidatorSet,
        target: ValidatorConsensusTarget,
        timeouts: BftTimeoutConfig,
    ) -> Result<(), BftConsensusRuntimeError> {
        // A collected body registers only the existing membership scope's Nil
        // round. The public proposal/signing APIs still reject unsealed roots.
        let collection =
            governance::collection_subject(&store, &validator_set, &target, signer.validator_id())?;
        let collection_round = collection.as_ref().map(|(_, round)| *round);
        let subject = match collection {
            Some((subject, _)) => subject,
            None => target.proposal_subject(&store)?,
        };
        let scope = subject.scope().clone();
        if let Some(existing) = self
            .recent_completed
            .iter()
            .find(|completed| completed.subject.scope() == &scope)
        {
            if existing.subject == subject {
                return Ok(());
            }
            return Err(BftConsensusRuntimeError::SubjectConflict(scope));
        }
        if let Some(existing) = self.sessions.get_mut(&scope) {
            let starts_membership = collection_round.is_none()
                && existing.collection_started_at_round.is_some()
                && matches!(target, ValidatorConsensusTarget::ValidatorSetTransition(_))
                && !existing.candidates.contains_key(&subject.digest())
                && existing.driver.phase()? == BftDriverPhase::Proposal;
            if existing.subject != subject || existing.target != target {
                if !matches!(
                    subject.scope(),
                    ConsensusScope::CurrencyAllocation { .. }
                        | ConsensusScope::StateRecoveryCheckpoint { .. }
                        | ConsensusScope::PreparedTask(_)
                ) {
                    return Err(BftConsensusRuntimeError::SubjectConflict(scope));
                }
                if !existing.candidates.contains_key(&subject.digest())
                    && existing.candidates.len() >= MAX_PENDING_MESSAGES_PER_SCOPE
                {
                    return Err(BftConsensusRuntimeError::PendingFutureMessagesFull(scope));
                }
                existing.driver.register_subject(&subject)?;
                existing.candidates.insert(subject.digest(), target);
            }
            // A failed start has no deadline. A newly validated registration
            // may resume it; active sessions keep their original timeout and
            // finished sessions are never restarted here.
            existing.needs_start |=
                !existing.finished && (existing.deadline.is_none() || starts_membership);
            if existing.collection_started_at_round.is_none() {
                existing.collection_started_at_round = collection_round;
            }
            return Ok(());
        }

        let mut driver = BftDriver::new(
            signer.clone(),
            store.clone(),
            validator_set.clone(),
            scope.clone(),
        )?;
        driver.register_subject(&subject)?;
        let valid_prevote_qc = store
            .bft_local_state(signer.validator_id(), &scope)
            .map_err(BftConsensusRuntimeError::Persistence)?
            .and_then(|state| state.valid_prevote_qc().cloned());
        self.sessions.insert(
            scope.clone(),
            BftConsensusSession {
                driver,
                store,
                signer,
                validator_set,
                target,
                candidates: BTreeMap::new(),
                valid_prevote_qc,
                subject,
                finality_votes: BTreeMap::new(),
                finality_qc: None,
                finality_certificate: None,
                relayed_finality_certificate: false,
                bft_finality_ready: false,
                certified_emitted: false,
                timeouts,
                deadline: None,
                relayed_certificates: BTreeSet::new(),
                pending_future: VecDeque::new(),
                needs_start: true,
                collection_started_at_round: collection_round,
                finished: false,
            },
        );
        if let Some(session) = self.sessions.get_mut(&scope)
            && matches!(
                session.subject.scope(),
                ConsensusScope::CurrencyAllocation { .. }
                    | ConsensusScope::StateRecoveryCheckpoint { .. }
                    | ConsensusScope::PreparedTask(_)
            )
        {
            session
                .candidates
                .insert(session.subject.digest(), session.target.clone());
        }
        if let Some(session) = self.sessions.get_mut(&scope)
            && let ConsensusScope::PreparedTask(task_id) = &scope
        {
            let commit = session
                .store
                .prepared_bft_proposal_subject(task_id.clone())
                .map_err(BftConsensusRuntimeError::Persistence)?;
            session.driver.register_subject(&commit)?;
            session.candidates.insert(
                commit.digest(),
                ValidatorConsensusTarget::PreparedTask {
                    task_id: task_id.clone(),
                    plan_digest: commit.digest(),
                },
            );
        }
        Ok(())
    }

    // Events are diagnostics; task outcomes and certificates remain durable authority.
    fn record_event(&mut self, event: BftConsensusEvent) {
        const MAX_RECENT_EVENTS: usize = 256;
        if self.events.len() == MAX_RECENT_EVENTS {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    pub(crate) fn drain_events(&mut self) -> Vec<BftConsensusEvent> {
        self.events.drain(..).collect()
    }

    pub(crate) fn record_send_failures(
        &mut self,
        scope: ConsensusScope,
        failures: Vec<ValidatorBftSendFailure>,
    ) {
        if failures.is_empty() {
            return;
        }
        self.record_event(BftConsensusEvent::SendFailed { scope, failures });
    }

    pub(crate) fn record_rejection(
        &mut self,
        validator_id: Option<ValidatorId>,
        scope: ConsensusScope,
        error: BftConsensusRuntimeError,
    ) {
        self.record_event(BftConsensusEvent::Rejected {
            validator_id,
            scope,
            error,
        });
    }

    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.sessions
            .values()
            .filter(|session| !session.finished)
            .filter_map(|session| session.deadline)
            .min()
    }

    pub(crate) fn drive(
        &mut self,
        inbound: Vec<InboundBftMessage>,
        now: Instant,
    ) -> BftConsensusOutput {
        let mut output = BftConsensusOutput::default();
        let mut inbound = self.take_registered_pending(inbound);

        let pending = self
            .sessions
            .iter()
            .filter_map(|(scope, session)| session.needs_start.then_some(scope.clone()))
            .collect::<Vec<_>>();
        for scope in pending {
            let result = self.with_session(
                &scope,
                |session, output| {
                    session.needs_start = false;
                    start_round(session, output, now)
                },
                &mut output,
            );
            if let Err(error) = result {
                self.record_event(BftConsensusEvent::Rejected {
                    validator_id: None,
                    scope,
                    error,
                });
            }
        }

        for inbound_message in inbound.drain(..) {
            self.dispatch_inbound(inbound_message, now, &mut output);
        }

        let due = self
            .sessions
            .iter()
            .filter_map(|(scope, session)| {
                (!session.finished && session.deadline.is_some_and(|deadline| deadline <= now))
                    .then_some(scope.clone())
            })
            .collect::<Vec<_>>();
        for scope in due {
            let result = self.with_session(
                &scope,
                |session, output| {
                    if session.bft_finality_ready || session.finality_certificate.is_some() {
                        // Retry interrupted signing/commit before relaying; an expired
                        // deadline must not spin if persistence rejects this attempt.
                        schedule_finality_relay(session, now);
                        let mut event = finality::certify_if_ready(session, output)?;
                        if !session.certified_emitted
                            && !session
                                .finality_votes
                                .contains_key(&session.signer.validator_id())
                        {
                            event = begin_business_finality(session, output, now)?;
                        }
                        if event.is_none() {
                            rebroadcast_finality_votes(session, output)?;
                        }
                        return Ok(event);
                    }

                    let action = session.driver.on_timeout()?;
                    let event = process_action(session, action, output, now)?;
                    if event.is_none() {
                        refresh_deadline(session, now)?;
                    }
                    Ok(event)
                },
                &mut output,
            );
            if let Err(error) = result {
                self.record_event(BftConsensusEvent::Rejected {
                    validator_id: None,
                    scope,
                    error,
                });
            }
        }

        self.replay_ready_future(now, &mut output);
        self.retire_finished_sessions(now);
        output
    }

    fn retire_finished_sessions(&mut self, now: Instant) {
        let finished = self
            .sessions
            .iter()
            .filter_map(|(scope, session)| session.finished.then_some(scope.clone()))
            .collect::<Vec<_>>();

        for scope in finished {
            let Some(session) = self.sessions.remove(&scope) else {
                continue;
            };
            self.pending_unregistered.remove(&scope);
            let Some(certificate) = session.finality_certificate else {
                continue;
            };
            self.remember_completed(CompletedConsensusScope {
                recovery_source: match &session.target {
                    ValidatorConsensusTarget::StateRecoveryCheckpoint(checkpoint) => {
                        governance::recovery_proof_message(checkpoint, &certificate).ok()
                    }
                    _ => None,
                },
                transition_source: match &session.target {
                    ValidatorConsensusTarget::ValidatorSetTransition(transition) => {
                        ValidatorSetTransitionSource::from_transition(transition)
                            .encode_bytes()
                            .ok()
                    }
                    _ => None,
                },
                allocation_source: match session.target {
                    ValidatorConsensusTarget::CurrencyAllocation(allocation) => {
                        Some(allocation.task)
                    }
                    _ => None,
                },
                subject: session.subject,
                validator_set: session.validator_set,
                certificate,
                relay_interval: session.timeouts.precommit,
                next_relay_at: now,
            });
        }
    }

    fn remember_completed(&mut self, completed: CompletedConsensusScope) {
        if let Some(index) = self
            .recent_completed
            .iter()
            .position(|existing| existing.subject.scope() == completed.subject.scope())
        {
            self.recent_completed.remove(index);
        }
        if self.recent_completed.len() >= MAX_RECENT_COMPLETED_SCOPES {
            self.recent_completed.pop_front();
        }
        self.recent_completed.push_back(completed);
    }

    fn dispatch_inbound(
        &mut self,
        inbound_message: InboundBftMessage,
        now: Instant,
        output: &mut BftConsensusOutput,
    ) {
        let scope = inbound_message.message.scope().clone();
        let validator_id = inbound_message.validator_id;
        if let Some(completed) = self
            .recent_completed
            .iter_mut()
            .find(|completed| completed.subject.scope() == &scope)
        {
            if !matches!(
                inbound_message.message,
                BftNetworkMessage::FinalityCertificate { .. }
            ) && now >= completed.next_relay_at
            {
                output
                    .outbound
                    .push(completed.recovery_source.clone().unwrap_or_else(|| {
                        BftNetworkMessage::FinalityCertificate {
                            scope,
                            certificate: completed.certificate.clone(),
                        }
                    }));
                completed.next_relay_at = add_duration(now, completed.relay_interval);
            }
            return;
        }
        if !self.sessions.contains_key(&scope) {
            self.buffer_unregistered(inbound_message);
            return;
        }

        let future_scope = scope.clone();
        let result = self.with_session(
            &scope,
            move |session, output| {
                candidates::admit_certified_abort(session, &inbound_message.message)?;
                if candidates::waiting_for_candidate(session, &inbound_message.message) {
                    if !session.pending_future.contains(&inbound_message) {
                        if session.pending_future.len() >= MAX_PENDING_MESSAGES_PER_SCOPE {
                            return Err(BftConsensusRuntimeError::PendingFutureMessagesFull(
                                future_scope,
                            ));
                        }
                        session.pending_future.push_back(inbound_message);
                    }
                    return Ok(None);
                }
                if let Some(actual) = inbound_message.message.consensus_round() {
                    let current = session.driver.current_round()?;
                    if actual < current && !inbound_message.message.is_digest_precommit_evidence() {
                        if let BftNetworkMessage::QuorumCertificate(certificate) =
                            &inbound_message.message
                            && certificate.statement().phase() == BftPhase::Prevote
                            && matches!(certificate.statement().value(), BftValue::Digest(_))
                        {
                            certificate
                                .verify(&session.validator_set)
                                .map_err(BftDriverError::from)?;
                            session
                                .store
                                .remember_verified_bft_prevote_qc(
                                    session.driver.validator_id(),
                                    certificate,
                                    &session.validator_set,
                                )
                                .map_err(BftConsensusRuntimeError::Persistence)?;
                            candidates::remember_prevote_qc(session, certificate);
                        }
                        return Ok(None);
                    }
                    if actual > current
                        && !matches!(
                            &inbound_message.message,
                            BftNetworkMessage::QuorumCertificate(_)
                        )
                    {
                        if session.pending_future.contains(&inbound_message) {
                            return Ok(None);
                        }
                        if session.pending_future.len() >= MAX_PENDING_MESSAGES_PER_SCOPE {
                            return Err(BftConsensusRuntimeError::PendingFutureMessagesFull(
                                future_scope,
                            ));
                        }
                        session.pending_future.push_back(inbound_message);
                        return Ok(None);
                    }
                }
                handle_inbound(session, inbound_message.message, output, now)
            },
            output,
        );
        if let Err(error) = result {
            self.record_event(BftConsensusEvent::Rejected {
                validator_id: Some(validator_id),
                scope,
                error,
            });
        }
    }

    fn replay_ready_future(&mut self, now: Instant, output: &mut BftConsensusOutput) {
        loop {
            let scopes = self.sessions.keys().cloned().collect::<Vec<_>>();
            let mut ready = Vec::new();

            for scope in scopes {
                let Some(session) = self.sessions.get_mut(&scope) else {
                    continue;
                };
                if session.finished || session.pending_future.is_empty() {
                    continue;
                }

                let current = match session.driver.current_round() {
                    Ok(current) => current,
                    Err(error) => {
                        self.record_event(BftConsensusEvent::Rejected {
                            validator_id: None,
                            scope,
                            error: error.into(),
                        });
                        continue;
                    }
                };

                let mut future = VecDeque::new();
                while let Some(message) = session.pending_future.pop_front() {
                    if message
                        .message
                        .consensus_round()
                        .is_none_or(|round| round <= current)
                        && !candidates::waiting_for_candidate(session, &message.message)
                    {
                        ready.push(message);
                    } else {
                        future.push_back(message);
                    }
                }
                session.pending_future = future;
            }

            if ready.is_empty() {
                break;
            }
            for message in ready {
                self.dispatch_inbound(message, now, output);
            }
        }
    }

    fn take_registered_pending(
        &mut self,
        inbound: Vec<InboundBftMessage>,
    ) -> Vec<InboundBftMessage> {
        let ready_scopes = self
            .pending_unregistered
            .keys()
            .filter(|scope| self.sessions.contains_key(*scope))
            .cloned()
            .collect::<Vec<_>>();
        let mut ready = Vec::new();
        for scope in ready_scopes {
            if let Some(mut pending) = self.pending_unregistered.remove(&scope) {
                ready.extend(pending.drain(..));
            }
        }
        ready.extend(inbound);
        ready
    }

    fn buffer_unregistered(&mut self, message: InboundBftMessage) {
        let scope = message.message.scope().clone();
        if !self.pending_unregistered.contains_key(&scope)
            && self.pending_unregistered.len() >= MAX_PENDING_UNREGISTERED_SCOPES
        {
            self.record_event(BftConsensusEvent::UnregisteredScope {
                validator_id: message.validator_id,
                scope,
            });
            return;
        }

        let pending = self.pending_unregistered.entry(scope.clone()).or_default();
        if pending.len() >= MAX_PENDING_MESSAGES_PER_SCOPE {
            self.record_event(BftConsensusEvent::UnregisteredScope {
                validator_id: message.validator_id,
                scope,
            });
            return;
        }
        pending.push_back(message);
    }

    fn with_session<F>(
        &mut self,
        scope: &ConsensusScope,
        operation: F,
        output: &mut BftConsensusOutput,
    ) -> Result<(), BftConsensusRuntimeError>
    where
        F: FnOnce(
            &mut BftConsensusSession,
            &mut BftConsensusOutput,
        ) -> Result<Option<BftConsensusEvent>, BftConsensusRuntimeError>,
    {
        let Some(session) = self.sessions.get_mut(scope) else {
            return Ok(());
        };
        if session.finished {
            return Ok(());
        }
        if let Some(event) = operation(session, output)? {
            self.record_event(event);
        }
        Ok(())
    }
}

fn start_round(
    session: &mut BftConsensusSession,
    output: &mut BftConsensusOutput,
    now: Instant,
) -> Result<Option<BftConsensusEvent>, BftConsensusRuntimeError> {
    let durable = session
        .store
        .load_shared()
        .map_err(BftConsensusRuntimeError::Persistence)?
        .ok_or(BftConsensusRuntimeError::Persistence(
            PersistenceError::MissingSnapshot,
        ))?;
    let scope = session.subject.scope();
    if durable
        .validator_vote_locks
        .get(&(session.signer.validator_id(), scope.clone()))
        == Some(&session.subject.digest())
        || durable
            .bft_local_states
            .get(&(session.signer.validator_id(), scope.clone()))
            .is_some_and(|state| state.finality_ready_digest() == Some(session.subject.digest()))
    {
        return begin_business_finality(session, output, now);
    }
    if matches!(
        session.subject.scope(),
        ConsensusScope::CurrencyAllocation { .. }
            | ConsensusScope::StateRecoveryCheckpoint { .. }
            | ConsensusScope::PreparedTask(_)
    ) {
        let local = session
            .store
            .bft_local_state(session.driver.validator_id(), session.subject.scope())
            .map_err(BftConsensusRuntimeError::Persistence)?;
        let digest = session
            .valid_prevote_qc
            .as_ref()
            .filter(|certificate| {
                local
                    .as_ref()
                    .and_then(|state| state.locked_round())
                    .is_none_or(|round| certificate.statement().round() >= round)
            })
            .and_then(|certificate| match certificate.statement().value() {
                BftValue::Digest(digest) => Some(digest),
                _ => None,
            })
            .or_else(|| local.and_then(|state| state.locked_digest()));
        if let Some(digest) = digest {
            candidates::select_candidate(session, digest)?;
        } else if let ConsensusScope::PreparedTask(task_id) = session.subject.scope()
            && durable
                .prepared_tasks
                .get(task_id)
                .is_some_and(|plan| plan.conflict_abort)
        {
            let abort = session
                .store
                .prepared_abort_statement(task_id)
                .map_err(BftConsensusRuntimeError::Persistence)?;
            if session.candidates.contains_key(&abort.subject_digest()) {
                candidates::select_candidate(session, abort.subject_digest())?;
            }
        } else if let ConsensusScope::PreparedTask(task_id) = session.subject.scope()
            && let Some(plan) = durable
                .prepared_tasks
                .get(task_id)
                .filter(|plan| plan.commit_authorized)
        {
            let digest = plan.plan_digest().map_err(|_| {
                BftConsensusRuntimeError::Persistence(PersistenceError::InvalidSnapshot)
            })?;
            if session.candidates.contains_key(&digest) {
                candidates::select_candidate(session, digest)?;
            }
        } else if matches!(
            session.subject.scope(),
            ConsensusScope::StateRecoveryCheckpoint { .. }
        ) {
            let checkpoint = session
                .store
                .next_state_recovery_checkpoint()
                .map_err(BftConsensusRuntimeError::Persistence)?;
            if session.candidates.contains_key(&checkpoint.digest()) {
                candidates::select_candidate(session, checkpoint.digest())?;
            }
        } else if matches!(
            session.target,
            ValidatorConsensusTarget::ValidatorSetTransition(_)
        ) {
            candidates::select_membership_union(session, &durable)?;
        }
    }
    session.driver.register_subject(&session.subject)?;
    if !governance::membership_ready(session, &durable)? {
        refresh_deadline(session, now)?;
        return Ok(None);
    }
    // Restart resumes the durable phase. Re-proposing after a persisted Nil
    // prevote would try to sign a conflicting value and leave no deadline.
    if session.driver.phase()? != BftDriverPhase::Proposal {
        refresh_deadline(session, now)?;
        return Ok(None);
    }
    if session.driver.proposer()? == session.driver.validator_id() {
        match &session.target {
            ValidatorConsensusTarget::CurrencyAllocation(_) => {
                output
                    .outbound
                    .push(BftNetworkMessage::PreparedTaskAvailable {
                        validator_set_version: session.validator_set.version(),
                        scope: session.subject.scope().clone(),
                        expected_plan_digest: session.subject.digest(),
                        round: session.driver.current_round()?,
                    });
            }
            ValidatorConsensusTarget::ValidatorSetTransition(transition) => {
                let source = ValidatorSetTransitionSource::from_transition(transition)
                    .encode_bytes()
                    .map_err(BftConsensusRuntimeError::GovernanceSourceCodec)?;
                output
                    .outbound
                    .push(BftNetworkMessage::ValidatorSetTransitionSource {
                        collecting: false,
                        validator_set_version: session.validator_set.version(),
                        scope: session.subject.scope().clone(),
                        bytes: source,
                    });
            }
            ValidatorConsensusTarget::StateRecoveryCheckpoint(checkpoint) => {
                output
                    .outbound
                    .push(BftNetworkMessage::StateRecoveryCheckpointSource {
                        validator_set_version: session.validator_set.version(),
                        scope: session.subject.scope().clone(),
                        bytes: checkpoint
                            .encode_source()
                            .map_err(BftConsensusRuntimeError::Persistence)?,
                    });
            }
            ValidatorConsensusTarget::PublicCheckpoint(checkpoint) => {
                let snapshot = session
                    .store
                    .load_shared()
                    .map_err(BftConsensusRuntimeError::Persistence)?
                    .ok_or(BftConsensusRuntimeError::Persistence(
                        PersistenceError::MissingSnapshot,
                    ))?;
                if let Some(proof) = snapshot.public_checkpoint_baseline.as_ref()
                    && proof.validator_set_version() == session.validator_set.version()
                {
                    output
                        .outbound
                        .push(BftNetworkMessage::PublicCheckpointSource {
                            validator_set_version: session.validator_set.version(),
                            scope: ConsensusScope::PublicCheckpoint {
                                validator_set_version: session.validator_set.version(),
                                epoch: proof.checkpoint().epoch(),
                            },
                            bytes: proof
                                .encode_bytes()
                                .map_err(|_| BftConsensusRuntimeError::InvalidGovernanceSource)?,
                        });
                }
                output
                    .outbound
                    .push(BftNetworkMessage::PublicCheckpointSource {
                        validator_set_version: session.validator_set.version(),
                        scope: session.subject.scope().clone(),
                        bytes: checkpoint.encode_source(session.validator_set.version()),
                    });
            }
            ValidatorConsensusTarget::PreparedTask { .. } => {}
        }
        let proposal = session.driver.create_proposal(&session.subject)?;
        let unlock_certificate = session
            .valid_prevote_qc
            .as_ref()
            .filter(|certificate| {
                certificate.statement().round() < proposal.round()
                    && certificate.statement().value() == BftValue::Digest(session.subject.digest())
            })
            .cloned();
        output.outbound.push(BftNetworkMessage::Proposal {
            proposal: proposal.clone(),
            unlock_certificate: unlock_certificate.clone(),
        });
        let action = session.driver.accept_proposal(
            &proposal,
            &session.subject,
            unlock_certificate.as_ref(),
        )?;
        if let Some(event) = process_action(session, action, output, now)? {
            return Ok(Some(event));
        }
    }
    refresh_deadline(session, now)?;
    Ok(None)
}

fn handle_inbound(
    session: &mut BftConsensusSession,
    message: BftNetworkMessage,
    output: &mut BftConsensusOutput,
    now: Instant,
) -> Result<Option<BftConsensusEvent>, BftConsensusRuntimeError> {
    let message = match message {
        BftNetworkMessage::FinalityCertificate { scope, certificate } => {
            certificate
                .verify(&session.validator_set)
                .map_err(BftConsensusRuntimeError::Finality)?;
            candidates::select_candidate(session, certificate.statement().subject_digest())?;
            return ingest_finality_certificate(session, scope, certificate, output, now);
        }
        BftNetworkMessage::FinalityVote {
            scope,
            statement,
            vote,
        } => {
            return ingest_finality_vote(session, scope, statement, vote, output, now);
        }
        message => message,
    };
    if session.bft_finality_ready || session.finality_certificate.is_some() {
        return Ok(None);
    }

    let before = (session.driver.current_round()?, session.driver.phase()?);
    let action = match message {
        BftNetworkMessage::Proposal {
            proposal,
            unlock_certificate,
        } => {
            let current = session.driver.current_round()?;
            if proposal.round() == current && session.driver.phase()? != BftDriverPhase::Proposal {
                None
            } else {
                let subject = match session.candidates.get(&proposal.subject_digest()) {
                    Some(ValidatorConsensusTarget::PreparedTask {
                        task_id,
                        plan_digest,
                    }) => BftProposalSubject::new(
                        session.validator_set.version(),
                        ConsensusScope::PreparedTask(task_id.clone()),
                        *plan_digest,
                    ),
                    Some(ValidatorConsensusTarget::CurrencyAllocation(allocation)) => {
                        allocation.subject()
                    }
                    Some(ValidatorConsensusTarget::ValidatorSetTransition(transition)) => {
                        BftProposalSubject::new(
                            transition.current_validator_set_version(),
                            transition.scope(),
                            transition.digest(),
                        )
                    }
                    Some(ValidatorConsensusTarget::StateRecoveryCheckpoint(checkpoint)) => {
                        BftProposalSubject::new(
                            checkpoint.validator_set_version(),
                            ConsensusScope::StateRecoveryCheckpoint {
                                validator_set_version: checkpoint.validator_set_version(),
                                serial: checkpoint.serial(),
                            },
                            checkpoint.digest(),
                        )
                    }
                    _ => session.subject.clone(),
                };
                let action = session.driver.accept_proposal(
                    &proposal,
                    &subject,
                    unlock_certificate.as_ref(),
                )?;
                candidates::select_candidate(session, proposal.subject_digest())?;
                Some(action)
            }
        }
        BftNetworkMessage::Vote { statement, vote } => {
            session.driver.ingest_vote(statement, vote)?
        }
        BftNetworkMessage::QuorumCertificate(certificate) => {
            certificate
                .verify(&session.validator_set)
                .map_err(BftDriverError::from)?;
            if let BftValue::Digest(digest) = certificate.statement().value() {
                candidates::select_candidate(session, digest)?;
            }
            session.driver.register_subject(&session.subject)?;
            remember_finality_qc(session, &certificate);
            candidates::remember_prevote_qc(session, &certificate);
            let action = session
                .driver
                .accept_verified_quorum_certificate(&certificate)?;
            relay_certificate(session, &certificate, output);
            if let Some(event) = process_action(session, action, output, now)? {
                return Ok(Some(event));
            }
            refresh_deadline_if_progressed(session, before, now)?;
            return Ok(None);
        }
        BftNetworkMessage::FinalityVote { .. } | BftNetworkMessage::FinalityCertificate { .. } => {
            unreachable!("handled before BFT dispatch")
        }
        BftNetworkMessage::PreparedTaskRequest { .. }
        | BftNetworkMessage::PreparedTaskSourceChunk { .. }
        | BftNetworkMessage::PreparedTaskSourceUnavailable { .. }
        | BftNetworkMessage::PreparedTaskAvailable { .. } => {
            unreachable!("prepared-task sync control is consumed before BFT dispatch")
        }
        BftNetworkMessage::ValidatorSetTransitionSource { .. }
        | BftNetworkMessage::PublicCheckpointSource { .. }
        | BftNetworkMessage::StateRecoveryCheckpointSource { .. } => {
            unreachable!("governance source control is consumed before BFT dispatch")
        }
    };

    if let Some(action) = action
        && let Some(event) = process_action(session, action, output, now)?
    {
        return Ok(Some(event));
    }
    refresh_deadline_if_progressed(session, before, now)?;
    Ok(None)
}

fn process_action(
    session: &mut BftConsensusSession,
    action: BftDriverAction,
    output: &mut BftConsensusOutput,
    now: Instant,
) -> Result<Option<BftConsensusEvent>, BftConsensusRuntimeError> {
    match action {
        BftDriverAction::Noop => {}
        BftDriverAction::Vote { statement, vote } => {
            output.outbound.push(BftNetworkMessage::Vote {
                statement: statement.clone(),
                vote: vote.clone(),
            });
            if let Some(next) = session.driver.ingest_vote(statement, vote)?
                && let Some(event) = process_action(session, next, output, now)?
            {
                return Ok(Some(event));
            }
        }
        BftDriverAction::QuorumCertificate(certificate) => {
            if let BftValue::Digest(digest) = certificate.statement().value() {
                candidates::select_candidate(session, digest)?;
            }
            remember_finality_qc(session, &certificate);
            candidates::remember_prevote_qc(session, &certificate);
            relay_certificate(session, &certificate, output);
            // ingest_vote constructs this QC only from verified votes.
            let next = session
                .driver
                .accept_verified_quorum_certificate(&certificate)?;
            if let Some(event) = process_action(session, next, output, now)? {
                return Ok(Some(event));
            }
        }
        BftDriverAction::FinalityReady { digest, .. } => {
            if digest != session.subject.digest() {
                return Err(BftConsensusRuntimeError::FinalityStatementMismatch);
            }
            return begin_business_finality(session, output, now);
        }
        BftDriverAction::RoundAdvanced { .. } => {
            session.relayed_certificates.clear();
            return start_round(session, output, now);
        }
    }
    Ok(None)
}

fn relay_certificate(
    session: &mut BftConsensusSession,
    certificate: &BftQuorumCertificate,
    output: &mut BftConsensusOutput,
) {
    let statement = certificate.statement();
    let key = (statement.round(), statement.phase(), statement.value());
    if session.relayed_certificates.insert(key) {
        output
            .outbound
            .push(BftNetworkMessage::QuorumCertificate(certificate.clone()));
    }
}

fn refresh_deadline_if_progressed(
    session: &mut BftConsensusSession,
    before: (u64, BftDriverPhase),
    now: Instant,
) -> Result<(), BftConsensusRuntimeError> {
    let after = (session.driver.current_round()?, session.driver.phase()?);
    if after != before {
        refresh_deadline(session, now)?;
    }
    Ok(())
}

fn refresh_deadline(
    session: &mut BftConsensusSession,
    now: Instant,
) -> Result<(), BftConsensusRuntimeError> {
    if session.bft_finality_ready {
        return Ok(());
    }
    let base_delay = match session.driver.phase()? {
        BftDriverPhase::Proposal => session.timeouts.proposal,
        BftDriverPhase::Prevote => session.timeouts.prevote,
        BftDriverPhase::Precommit => session.timeouts.precommit,
    };
    // Fixed budgets can keep timing out after network/processing latency grows.
    // Each scope uses its durable round; there is no second timer state.
    // Grow after a complete proposer rotation; larger committees need more
    // time for the unavoidable quorum verification work.
    let members = session.validator_set.len() as u64;
    let cycle = (session.driver.current_round()? / members).saturating_add(1);
    let quorum_work = session.validator_set.quorum_threshold().div_ceil(3) as u64;
    let factor = u32::try_from(cycle.saturating_mul(quorum_work)).unwrap_or(u32::MAX);
    let delay = base_delay.saturating_mul(factor);
    // drive's timestamp can predate expensive verification/persistence work.
    session.deadline = Some(add_duration(now.max(Instant::now()), delay));
    Ok(())
}

fn add_duration(now: Instant, duration: Duration) -> Instant {
    let mut duration = duration;
    loop {
        if let Some(deadline) = now.checked_add(duration) {
            return deadline;
        }
        // Overflow must not turn a distant deadline into an immediate wakeup.
        duration /= 2;
    }
}

#[cfg(test)]
mod task_decision_tests;
#[cfg(test)]
mod tests;
