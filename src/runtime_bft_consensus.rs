use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::Notify;
use tokio::time::Instant;

mod finality;

use finality::{
    begin_business_finality, ingest_finality_certificate, ingest_finality_vote,
    rebroadcast_finality_votes, remember_finality_qc, schedule_finality_relay,
};

use crate::runtime_bft::InboundBftMessage;
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
    ValidatorSetTransition, ValidatorSigner, ValidatorSigningError, ValidatorTransitionError,
    ValidatorVote,
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

    pub(crate) fn wake(&self) {
        self.activity.notify_one();
    }

    pub(crate) async fn wait_for_activity_or_deadline(&self) {
        let deadline = self
            .coordinator
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .next_deadline();
        match deadline {
            Some(deadline) => {
                tokio::select! {
                    _ = tokio::time::sleep_until(deadline) => {}
                    _ = self.activity.notified() => {}
                }
            }
            None => self.activity.notified().await,
        }
    }
}

impl NodeRuntime {
    pub fn start_prepared_task_consensus(&self, task_id: TaskId) -> Result<(), NodeRuntimeError> {
        let target = ValidatorConsensusTarget::prepared_task(&self.store, task_id)
            .map_err(BftConsensusRuntimeError::from)?;
        self.start_validator_consensus_target(target)
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
        self.start_validator_consensus_target(ValidatorConsensusTarget::ValidatorSetTransition(
            transition,
        ))
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
        runtime.refresh_authority()?;
        let active_validator_set = runtime.validator_set();
        let validator_set = target
            .validator_set(&self.store, &active_validator_set)
            .map_err(BftConsensusRuntimeError::from)?;
        let signer = runtime.signer_for(&validator_set)?;
        let prepared_subject = matches!(target, ValidatorConsensusTarget::PreparedTask { .. })
            .then(|| target.proposal_subject(&self.store))
            .transpose()
            .map_err(BftConsensusRuntimeError::from)?;
        runtime.consensus().register(
            signer,
            self.store.clone(),
            validator_set.clone(),
            target,
            runtime.bft_timeouts(),
        )?;
        if let Some(subject) = prepared_subject {
            runtime.announce_prepared_task(
                &validator_set,
                subject.scope().clone(),
                subject.digest(),
            );
        }
        Ok(())
    }

    pub fn drain_bft_consensus_events(&self) -> Result<Vec<BftConsensusEvent>, NodeRuntimeError> {
        let runtime = self
            .validator_bft
            .as_ref()
            .ok_or(NodeRuntimeError::ValidatorBftNotConfigured)?;
        Ok(runtime.consensus().drain_events())
    }

    pub(crate) async fn run_validator_bft_consensus(&self) -> Result<(), NodeRuntimeError> {
        let Some(runtime) = self.validator_bft.as_ref() else {
            return std::future::pending::<Result<(), NodeRuntimeError>>().await;
        };
        self.start_durable_prepared_consensus()?;

        loop {
            let inbound = self.process_prepared_task_sync(runtime.drain_inbound())?;
            let output = runtime
                .consensus()
                .drive(inbound, tokio::time::Instant::now());
            for message in output.outbound {
                let scope = message.scope().clone();
                let failures = runtime.broadcast(&message);
                if !failures.is_empty() {
                    runtime.consensus().record_send_failures(scope, failures);
                }
            }

            if runtime.has_pending_prepared_task_sync() {
                tokio::select! {
                    _ = runtime.consensus().wait_for_activity_or_deadline() => {}
                    _ = tokio::time::sleep(runtime.bft_timeouts().proposal) => {
                        runtime.retry_prepared_task_sync(true);
                    }
                }
            } else {
                runtime.consensus().wait_for_activity_or_deadline().await;
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BftConsensusRuntimeError {
    Driver(BftDriverError),
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
    subject: BftProposalSubject,
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
    finished: bool,
}

#[derive(Default)]
pub(crate) struct BftConsensusOutput {
    pub(crate) outbound: Vec<BftNetworkMessage>,
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

    pub(crate) fn register(
        &mut self,
        signer: ValidatorSigner,
        store: crate::StateStore,
        validator_set: ValidatorSet,
        target: ValidatorConsensusTarget,
        timeouts: BftTimeoutConfig,
    ) -> Result<(), BftConsensusRuntimeError> {
        let subject = target.proposal_subject(&store)?;
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
        if let Some(existing) = self.sessions.get(&scope) {
            if existing.subject == subject && existing.target == target {
                return Ok(());
            }
            return Err(BftConsensusRuntimeError::SubjectConflict(scope));
        }

        let mut driver = BftDriver::new(
            signer.clone(),
            store.clone(),
            validator_set.clone(),
            scope.clone(),
        )?;
        driver.register_subject(&subject)?;
        self.sessions.insert(
            scope,
            BftConsensusSession {
                driver,
                store,
                signer,
                validator_set,
                target,
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
                finished: false,
            },
        );
        Ok(())
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
        self.events
            .push_back(BftConsensusEvent::SendFailed { scope, failures });
    }

    pub(crate) fn record_rejection(
        &mut self,
        validator_id: Option<ValidatorId>,
        scope: ConsensusScope,
        error: BftConsensusRuntimeError,
    ) {
        self.events.push_back(BftConsensusEvent::Rejected {
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
                self.events.push_back(BftConsensusEvent::Rejected {
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
                        rebroadcast_finality_votes(session, output);
                        if session.certified_emitted {
                            session.finished = true;
                            session.deadline = None;
                        } else {
                            schedule_finality_relay(session, now);
                        }
                        return Ok(None);
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
                self.events.push_back(BftConsensusEvent::Rejected {
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
                subject: session.subject,
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
                    .push(BftNetworkMessage::FinalityCertificate {
                        scope,
                        certificate: completed.certificate.clone(),
                    });
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
                if let Some(actual) = inbound_message.message.consensus_round() {
                    let current = session.driver.current_round()?;
                    if actual < current && !inbound_message.message.is_digest_precommit_evidence() {
                        return Ok(None);
                    }
                    if actual > current {
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
            self.events.push_back(BftConsensusEvent::Rejected {
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
                        self.events.push_back(BftConsensusEvent::Rejected {
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
            self.events.push_back(BftConsensusEvent::UnregisteredScope {
                validator_id: message.validator_id,
                scope,
            });
            return;
        }

        let pending = self.pending_unregistered.entry(scope.clone()).or_default();
        if pending.len() >= MAX_PENDING_MESSAGES_PER_SCOPE {
            self.events.push_back(BftConsensusEvent::UnregisteredScope {
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
            self.events.push_back(event);
        }
        Ok(())
    }
}

fn start_round(
    session: &mut BftConsensusSession,
    output: &mut BftConsensusOutput,
    now: Instant,
) -> Result<Option<BftConsensusEvent>, BftConsensusRuntimeError> {
    session.driver.register_subject(&session.subject)?;
    if session.driver.proposer()? == session.driver.validator_id() {
        let proposal = session.driver.create_proposal(&session.subject)?;
        output.outbound.push(BftNetworkMessage::Proposal {
            proposal: proposal.clone(),
            unlock_certificate: None,
        });
        let action = session
            .driver
            .accept_proposal(&proposal, &session.subject, None)?;
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
            return ingest_finality_certificate(session, scope, certificate, output);
        }
        BftNetworkMessage::FinalityVote {
            scope,
            statement,
            vote,
        } => {
            return ingest_finality_vote(session, scope, statement, vote, output);
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
                Some(session.driver.accept_proposal(
                    &proposal,
                    &session.subject,
                    unlock_certificate.as_ref(),
                )?)
            }
        }
        BftNetworkMessage::Vote { statement, vote } => {
            session.driver.ingest_vote(statement, vote)?
        }
        BftNetworkMessage::QuorumCertificate(certificate) => {
            session.driver.register_subject(&session.subject)?;
            remember_finality_qc(session, &certificate);
            let action = session.driver.accept_quorum_certificate(&certificate)?;
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
            remember_finality_qc(session, &certificate);
            relay_certificate(session, &certificate, output);
            let next = session.driver.accept_quorum_certificate(&certificate)?;
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
    let delay = match session.driver.phase()? {
        BftDriverPhase::Proposal => session.timeouts.proposal,
        BftDriverPhase::Prevote => session.timeouts.prevote,
        BftDriverPhase::Precommit => session.timeouts.precommit,
    };
    session.deadline = Some(add_duration(now, delay));
    Ok(())
}

fn add_duration(now: Instant, duration: Duration) -> Instant {
    now.checked_add(duration).unwrap_or(now)
}
