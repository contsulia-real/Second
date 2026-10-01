mod keys;

pub use keys::ValidatorRuntimeKeys;

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use tokio::sync::mpsc;

use crate::network::{
    MAX_CONCURRENT_ONE_WAY_STREAMS, MAX_PEER_RECORDS, NetworkError, NodeId, PeerRecord, QuicClient,
    QuicRequestStream, QuicTransportIdentity, SharedValidatorBftAuthority, ValidatorBftAuthority,
    ValidatorBftPeer, authenticate_validator_bft_peer_with_authority, outbound_bind_address,
    serve_validator_bft_connection_from_request,
};
use crate::runtime::{ActiveConnectionPermit, MAX_ACTIVE_CONNECTIONS};
use crate::runtime_bft_consensus::ValidatorConsensusRuntime;
use crate::{
    BftNetworkMessage, NodeRuntime, NodeRuntimeError, PersistenceError, StateStore, ValidatorId,
    ValidatorSet, ValidatorSigner,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InboundBftMessage {
    pub validator_id: ValidatorId,
    pub message: BftNetworkMessage,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatorBftSendFailure {
    pub validator_id: ValidatorId,
    pub error: NetworkError,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValidatorBftRuntimeError {
    UnknownValidator(ValidatorId),
    IdentityKeyMismatch(ValidatorId),
    ConsensusKeyMismatch(ValidatorId),
    SelfValidatorPeer(ValidatorId),
    Persistence(PersistenceError),
    Network(NetworkError),
}

impl From<PersistenceError> for ValidatorBftRuntimeError {
    fn from(value: PersistenceError) -> Self {
        Self::Persistence(value)
    }
}

impl From<NetworkError> for ValidatorBftRuntimeError {
    fn from(value: NetworkError) -> Self {
        Self::Network(value)
    }
}

#[derive(Clone)]
pub(crate) struct ValidatorBftRuntime {
    inner: Arc<ValidatorBftRuntimeInner>,
}

struct ValidatorBftRuntimeInner {
    keys: ValidatorRuntimeKeys,
    store: StateStore,
    authority: SharedValidatorBftAuthority,
    outbound: Mutex<BTreeMap<ValidatorId, ManagedValidatorBftPeer>>,
    inbound: Mutex<VecDeque<InboundBftMessage>>,
    consensus: ValidatorConsensusRuntime,
    rejected_nodes: Mutex<BTreeSet<NodeId>>,
}

const VALIDATOR_BFT_SEND_QUEUE_CAPACITY: usize = 64;
const VALIDATOR_FINALITY_SEND_QUEUE_CAPACITY: usize = 32;
const VALIDATOR_BFT_NORMAL_IN_FLIGHT_LIMIT: usize = MAX_CONCURRENT_ONE_WAY_STREAMS / 2;
const VALIDATOR_FINALITY_IN_FLIGHT_LIMIT: usize =
    MAX_CONCURRENT_ONE_WAY_STREAMS - VALIDATOR_BFT_NORMAL_IN_FLIGHT_LIMIT;

struct ManagedValidatorBftPeer {
    peer: ValidatorBftPeer,
    sender: mpsc::Sender<BftNetworkMessage>,
    finality_sender: mpsc::Sender<BftNetworkMessage>,
    alive: Arc<AtomicBool>,
    _client: QuicClient,
    _permit: ActiveConnectionPermit,
}

impl NodeRuntime {
    pub fn load_validator_and_bind(
        listen_address: std::net::SocketAddr,
        store: &StateStore,
        keys: ValidatorRuntimeKeys,
    ) -> Result<Self, NodeRuntimeError> {
        let mut runtime = Self::load_and_bind(listen_address, store)?;
        let persisted = runtime
            .store
            .load()?
            .ok_or(NodeRuntimeError::SnapshotMissing)?;
        runtime.validator_bft = Some(ValidatorBftRuntime::new(
            keys,
            runtime.store.clone(),
            persisted.validator_set,
            persisted.retained_validator_sets.into_values(),
        )?);
        Ok(runtime)
    }

    pub fn validator_id(&self) -> Option<ValidatorId> {
        self.validator_bft
            .as_ref()
            .map(ValidatorBftRuntime::validator_id)
    }

    pub fn connected_validator_ids(&self) -> Vec<ValidatorId> {
        self.validator_bft
            .as_ref()
            .map(ValidatorBftRuntime::connected_validator_ids)
            .unwrap_or_default()
    }

    pub(crate) async fn dial_validator_bft(
        &self,
        record: &PeerRecord,
    ) -> Result<ValidatorId, NodeRuntimeError> {
        let runtime = self
            .validator_bft
            .as_ref()
            .ok_or(NodeRuntimeError::ValidatorBftNotConfigured)?;
        runtime.refresh_authority()?;
        let permit = ActiveConnectionPermit::try_acquire(&self.active_connections).ok_or(
            NodeRuntimeError::ConnectionCapacityReached {
                maximum: MAX_ACTIVE_CONNECTIONS,
            },
        )?;
        Ok(runtime
            .dial(record, self.transport_identity.clone(), permit)
            .await?)
    }

    pub(crate) async fn maintain_validator_bft_peers(
        &self,
        bootstrap_records: &[PeerRecord],
    ) -> Result<(), NodeRuntimeError> {
        let Some(runtime) = self.validator_bft.as_ref() else {
            return Ok(());
        };
        runtime.refresh_authority()?;
        let target = runtime
            .validator_ids()
            .into_iter()
            .filter(|validator_id| *validator_id != runtime.validator_id())
            .count();
        if runtime.connected_validator_ids().len() >= target {
            return Ok(());
        }

        let connected_nodes = runtime.connected_node_ids();
        let rejected_nodes = runtime.rejected_node_ids();
        let mut seen = HashSet::new();
        let mut candidates = self.peer_store.recent(MAX_PEER_RECORDS, &[self.node_id()]);
        candidates.extend(
            bootstrap_records
                .iter()
                .filter(|record| record.node_id() != self.node_id())
                .cloned(),
        );

        for record in candidates {
            if runtime.connected_validator_ids().len() >= target {
                break;
            }
            if !seen.insert(record.node_id())
                || connected_nodes.contains(&record.node_id())
                || rejected_nodes.contains(&record.node_id())
            {
                continue;
            }

            match self.dial_validator_bft(&record).await {
                Ok(_) => {}
                Err(NodeRuntimeError::ValidatorBft(ValidatorBftRuntimeError::Network(
                    NetworkError::BftUnauthorized,
                ))) => runtime.reject_node(record.node_id()),
                Err(_) => {}
            }
        }
        Ok(())
    }
}

impl ValidatorBftRuntime {
    pub(crate) fn new(
        keys: ValidatorRuntimeKeys,
        store: StateStore,
        validator_set: ValidatorSet,
        retained_validator_sets: impl IntoIterator<Item = ValidatorSet>,
    ) -> Result<Self, ValidatorBftRuntimeError> {
        let authority = ValidatorBftAuthority::new(validator_set, retained_validator_sets)?;
        if authority.identity_public_key(keys.validator_id())
            != Some(keys.identity_key().verifying_key().to_bytes())
        {
            return if authority
                .validator_ids()
                .any(|validator_id| validator_id == keys.validator_id())
            {
                Err(ValidatorBftRuntimeError::IdentityKeyMismatch(
                    keys.validator_id(),
                ))
            } else {
                Err(ValidatorBftRuntimeError::UnknownValidator(
                    keys.validator_id(),
                ))
            };
        }

        Ok(Self {
            inner: Arc::new(ValidatorBftRuntimeInner {
                keys,
                store,
                authority: Arc::new(std::sync::RwLock::new(authority)),
                outbound: Mutex::new(BTreeMap::new()),
                inbound: Mutex::new(VecDeque::new()),
                consensus: ValidatorConsensusRuntime::new(),
                rejected_nodes: Mutex::new(BTreeSet::new()),
            }),
        })
    }

    pub(crate) fn validator_id(&self) -> ValidatorId {
        self.inner.keys.validator_id()
    }

    pub(crate) fn signer_for(
        &self,
        validator_set: &ValidatorSet,
    ) -> Result<ValidatorSigner, ValidatorBftRuntimeError> {
        let signing_key = self.inner.keys.consensus_key_for(validator_set).ok_or(
            ValidatorBftRuntimeError::ConsensusKeyMismatch(self.validator_id()),
        )?;
        Ok(ValidatorSigner::new(
            self.validator_id(),
            signing_key.clone(),
            self.inner.store.clone(),
        ))
    }

    pub(crate) fn validator_ids(&self) -> Vec<ValidatorId> {
        self.inner
            .authority
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .validator_ids()
            .collect()
    }

    pub(crate) fn validator_set(&self) -> ValidatorSet {
        self.inner
            .authority
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .active_validator_set()
            .clone()
    }

    pub(crate) fn refresh_authority(&self) -> Result<(), ValidatorBftRuntimeError> {
        let persisted = self
            .inner
            .store
            .load()?
            .ok_or(PersistenceError::MissingSnapshot)?;
        let authority = ValidatorBftAuthority::new(
            persisted.validator_set,
            persisted.retained_validator_sets.into_values(),
        )?;

        if let Some(identity_public_key) = authority.identity_public_key(self.validator_id())
            && identity_public_key != self.inner.keys.identity_key().verifying_key().to_bytes()
        {
            return Err(ValidatorBftRuntimeError::IdentityKeyMismatch(
                self.validator_id(),
            ));
        }

        let local_authorized = authority.identity_public_key(self.validator_id()).is_some();
        let allowed_validator_ids = if local_authorized {
            authority.validator_ids().collect::<BTreeSet<_>>()
        } else {
            BTreeSet::new()
        };

        *self
            .inner
            .authority
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = authority;

        let mut outbound = self
            .inner
            .outbound
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        outbound.retain(|validator_id, managed| {
            let keep = allowed_validator_ids.contains(validator_id);
            if !keep {
                managed.peer.close();
            }
            keep
        });
        drop(outbound);

        self.inner
            .rejected_nodes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        Ok(())
    }

    pub(crate) async fn serve_inbound(
        &self,
        peer: &crate::network::QuicPeer,
        first_request: QuicRequestStream,
    ) -> Result<ValidatorId, ValidatorBftRuntimeError> {
        self.refresh_authority()?;
        let inbound = Arc::clone(&self.inner);
        Ok(serve_validator_bft_connection_from_request(
            peer,
            first_request,
            self.inner.keys.validator_id(),
            self.inner.keys.identity_key(),
            &self.inner.authority,
            move |validator_id, message| {
                inbound
                    .inbound
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push_back(InboundBftMessage {
                        validator_id,
                        message,
                    });
                inbound.consensus.wake();
                Ok(())
            },
        )
        .await?)
    }

    pub(crate) async fn dial(
        &self,
        record: &PeerRecord,
        transport_identity: QuicTransportIdentity,
        permit: ActiveConnectionPermit,
    ) -> Result<ValidatorId, ValidatorBftRuntimeError> {
        let client = QuicClient::new(
            outbound_bind_address(record.address()),
            record.certificate_der(),
            transport_identity,
        )?;
        let peer = client
            .connect_expected(record.address(), record.node_id())
            .await?;
        let peer = authenticate_validator_bft_peer_with_authority(
            peer,
            self.inner.keys.validator_id(),
            self.inner.keys.identity_key(),
            &self.inner.authority,
        )
        .await?;
        let remote_validator_id = peer.remote_validator_id();
        if remote_validator_id == self.inner.keys.validator_id() {
            peer.close();
            return Err(ValidatorBftRuntimeError::SelfValidatorPeer(
                remote_validator_id,
            ));
        }

        let (sender, receiver) = mpsc::channel(VALIDATOR_BFT_SEND_QUEUE_CAPACITY);
        let (finality_sender, finality_receiver) =
            mpsc::channel(VALIDATOR_FINALITY_SEND_QUEUE_CAPACITY);
        let alive = Arc::new(AtomicBool::new(true));

        let mut outbound = self
            .inner
            .outbound
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        prune_dead_outbound(&mut outbound);
        if outbound.contains_key(&remote_validator_id) {
            peer.close();
            return Ok(remote_validator_id);
        }
        outbound.insert(
            remote_validator_id,
            ManagedValidatorBftPeer {
                peer: peer.clone(),
                sender,
                finality_sender,
                alive: Arc::clone(&alive),
                _client: client,
                _permit: permit,
            },
        );
        drop(outbound);

        tokio::spawn(run_validator_bft_sender(
            peer,
            remote_validator_id,
            receiver,
            finality_receiver,
            alive,
            Arc::downgrade(&self.inner),
        ));
        Ok(remote_validator_id)
    }

    pub(crate) fn connected_validator_ids(&self) -> Vec<ValidatorId> {
        let mut outbound = self
            .inner
            .outbound
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        prune_dead_outbound(&mut outbound);
        outbound.keys().copied().collect()
    }

    pub(crate) fn connected_node_ids(&self) -> BTreeSet<NodeId> {
        let mut outbound = self
            .inner
            .outbound
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        prune_dead_outbound(&mut outbound);
        outbound
            .values()
            .map(|managed| managed.peer.remote_node_id())
            .collect()
    }

    pub(crate) fn rejected_node_ids(&self) -> BTreeSet<NodeId> {
        self.inner
            .rejected_nodes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub(crate) fn reject_node(&self, node_id: NodeId) {
        self.inner
            .rejected_nodes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(node_id);
    }

    pub(crate) fn consensus(&self) -> &ValidatorConsensusRuntime {
        &self.inner.consensus
    }

    pub(crate) fn drain_inbound(&self) -> Vec<InboundBftMessage> {
        self.inner
            .inbound
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .drain(..)
            .collect()
    }

    pub(crate) fn broadcast(&self, message: &BftNetworkMessage) -> Vec<ValidatorBftSendFailure> {
        let mut outbound = self
            .inner
            .outbound
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        prune_dead_outbound(&mut outbound);

        let mut failures = Vec::new();
        let mut failed_ids = Vec::new();
        for (validator_id, managed) in outbound.iter() {
            if !managed.peer.accepts_recipient(message) {
                continue;
            }
            let sender = if matches!(
                message,
                BftNetworkMessage::FinalityVote { .. }
                    | BftNetworkMessage::FinalityCertificate { .. }
            ) {
                &managed.finality_sender
            } else {
                &managed.sender
            };
            if let Err(error) = sender.try_send(message.clone()) {
                managed.alive.store(false, Ordering::Release);
                let detail = match error {
                    mpsc::error::TrySendError::Full(_) => "validator BFT send queue is full",
                    mpsc::error::TrySendError::Closed(_) => "validator BFT send worker is closed",
                };
                failures.push(ValidatorBftSendFailure {
                    validator_id: *validator_id,
                    error: NetworkError::Transport(detail.to_owned()),
                });
                failed_ids.push(*validator_id);
            }
        }
        for validator_id in failed_ids {
            if let Some(managed) = outbound.remove(&validator_id) {
                managed.peer.close();
            }
        }
        failures
    }
}

fn prune_dead_outbound(outbound: &mut BTreeMap<ValidatorId, ManagedValidatorBftPeer>) {
    outbound.retain(|_, managed| {
        let alive = managed.alive.load(Ordering::Acquire);
        if !alive {
            managed.peer.close();
        }
        alive
    });
}

async fn run_validator_bft_sender(
    peer: ValidatorBftPeer,
    validator_id: ValidatorId,
    mut receiver: mpsc::Receiver<BftNetworkMessage>,
    mut finality_receiver: mpsc::Receiver<BftNetworkMessage>,
    alive: Arc<AtomicBool>,
    runtime: Weak<ValidatorBftRuntimeInner>,
) {
    let mut receiver_open = true;
    let mut finality_receiver_open = true;
    let mut normal_in_flight = 0_usize;
    let mut finality_in_flight = 0_usize;
    let mut in_flight = tokio::task::JoinSet::new();

    loop {
        if !receiver_open && !finality_receiver_open && in_flight.is_empty() {
            break;
        }

        tokio::select! {
            biased;

            message = finality_receiver.recv(),
                if finality_receiver_open
                    && finality_in_flight < VALIDATOR_FINALITY_IN_FLIGHT_LIMIT =>
            {
                match message {
                    Some(message) => {
                        finality_in_flight += 1;
                        let send_peer = peer.clone();
                        let scope = message.scope().clone();
                        in_flight.spawn(async move {
                            let result = send_peer.send(&message).await;
                            (true, Some(scope), result)
                        });
                    }
                    None => finality_receiver_open = false,
                }
            }

            result = in_flight.join_next(), if !in_flight.is_empty() => {
                let failure = match result {
                    Some(Ok((is_finality, scope, result))) => {
                        if is_finality {
                            finality_in_flight = finality_in_flight.saturating_sub(1);
                        } else {
                            normal_in_flight = normal_in_flight.saturating_sub(1);
                        }
                        result.err().map(|error| (scope, error))
                    }
                    Some(Err(error)) => Some((
                        None,
                        NetworkError::Transport(format!("BFT send task failed: {error}")),
                    )),
                    None => None,
                };
                if let Some((scope, error)) = failure {
                    alive.store(false, Ordering::Release);
                    if let (Some(runtime), Some(scope)) = (runtime.upgrade(), scope) {
                        runtime.consensus.record_send_failures(
                            scope,
                            vec![ValidatorBftSendFailure {
                                validator_id,
                                error,
                            }],
                        );
                        runtime.consensus.wake();
                    }
                    peer.close();
                    return;
                }
            }

            message = receiver.recv(),
                if receiver_open
                    && normal_in_flight < VALIDATOR_BFT_NORMAL_IN_FLIGHT_LIMIT =>
            {
                match message {
                    Some(message) => {
                        normal_in_flight += 1;
                        let send_peer = peer.clone();
                        let scope = message.scope().clone();
                        in_flight.spawn(async move {
                            let result = send_peer.send(&message).await;
                            (false, Some(scope), result)
                        });
                    }
                    None => receiver_open = false,
                }
            }
        }
    }

    alive.store(false, Ordering::Release);
    peer.close();
}
