use std::collections::{HashSet, VecDeque};
use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

pub(crate) const MAX_ACTIVE_CONNECTIONS: usize = 128;
const MAX_ACTIVE_LEGAL_TASK_SUBMISSIONS: usize = 8;
pub const DEFAULT_ACTIVE_PEER_TARGET: usize = 8;
const PEER_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(2);
const PEER_RETRY_INITIAL_DELAY: Duration = Duration::from_secs(1);
const PEER_RETRY_MAX_DELAY: Duration = Duration::from_secs(60);
const PEER_CHECKPOINT_QUERY_TIMEOUT: Duration = Duration::from_secs(5);
const PEER_PUBLIC_SYNC_TIMEOUT: Duration = Duration::from_secs(30);

use crate::network::{
    MAX_PEER_RECORDS, NetworkError, NetworkMessage, NodeId, PeerDirection, PeerLease, PeerManager,
    PeerRecord, PeerRegistrationError, PeerStore, PublicNetworkServices, QuicClient, QuicPeer,
    QuicRequestStream, QuicServer, QuicTransportIdentity, RuntimeNetworkSnapshot,
    RuntimePublicSnapshot, StateRecoveryProvider, StateRecoveryProviderHandle, client_peer_records,
    client_public_currency_checkpoint_proof,
    client_sync_certified_public_currency_view_from_checkpoint, new_state_recovery_provider_handle,
    outbound_bind_address, serve_public_network_connection,
    serve_public_network_connection_from_request, transport_identity_path,
};
use crate::runtime_bft::{
    ValidatorBftRuntime, ValidatorBftRuntimeError, ValidatorRuntimeConfig, ValidatorRuntimeKeys,
};
use crate::runtime_governance::serve_governance_request_from_request;
use crate::runtime_submission::serve_legal_task_submission_from_request;
use crate::{
    AuthorizationError, BftConsensusRuntimeError, BftDriverError, CertifiedStateRecoveryCheckpoint,
    PersistedNodeState, PersistedPublicNodeState, PersistenceError, PreparationError,
    PublicStateStore, RemoteCertifiedPublicCurrencyView, StateStore, TaskEncodingError,
};

#[derive(Debug)]
pub enum NodeRuntimeError {
    Persistence(PersistenceError),
    Authorization(AuthorizationError),
    Preparation(PreparationError),
    TaskEncoding(TaskEncodingError),
    PreparedTaskSourceTooLarge { maximum: usize, actual: usize },
    Network(NetworkError),
    SnapshotMissing,
    ConnectionCapacityReached { maximum: usize },
    SelfConnection,
    DuplicatePeer(NodeId),
    NoActivePeers,
    NoCertifiedPublicPeer,
    ValidatorBftNotConfigured,
    ValidatorBft(ValidatorBftRuntimeError),
    BftDriver(BftDriverError),
    BftConsensus(BftConsensusRuntimeError),
}

impl From<PersistenceError> for NodeRuntimeError {
    fn from(error: PersistenceError) -> Self {
        Self::Persistence(error)
    }
}

impl From<AuthorizationError> for NodeRuntimeError {
    fn from(error: AuthorizationError) -> Self {
        Self::Authorization(error)
    }
}

impl From<PreparationError> for NodeRuntimeError {
    fn from(error: PreparationError) -> Self {
        Self::Preparation(error)
    }
}

impl From<TaskEncodingError> for NodeRuntimeError {
    fn from(error: TaskEncodingError) -> Self {
        Self::TaskEncoding(error)
    }
}

impl From<NetworkError> for NodeRuntimeError {
    fn from(error: NetworkError) -> Self {
        Self::Network(error)
    }
}

impl From<ValidatorBftRuntimeError> for NodeRuntimeError {
    fn from(error: ValidatorBftRuntimeError) -> Self {
        Self::ValidatorBft(error)
    }
}

impl From<BftDriverError> for NodeRuntimeError {
    fn from(error: BftDriverError) -> Self {
        Self::BftDriver(error)
    }
}

impl From<BftConsensusRuntimeError> for NodeRuntimeError {
    fn from(error: BftConsensusRuntimeError) -> Self {
        Self::BftConsensus(error)
    }
}

#[derive(Clone)]
enum NodeStateBackend {
    Full(StateStore),
    Public(PublicStateStore),
}

impl NodeStateBackend {
    fn base_path(&self) -> &Path {
        match self {
            Self::Full(store) => store.base_path(),
            Self::Public(store) => store.base_path(),
        }
    }

    fn full_store(&self) -> Option<&StateStore> {
        match self {
            Self::Full(store) => Some(store),
            Self::Public(_) => None,
        }
    }
}

#[derive(Clone)]
struct PublicNetworkContext {
    backend: NodeStateBackend,
    peer_store: PeerStore,
    local_node_id: NodeId,
    local_peer_record: Option<PeerRecord>,
    state_recovery_provider: StateRecoveryProviderHandle,
}

#[derive(Default)]
pub struct NodeRuntimeCapabilities {
    validator: Option<(ValidatorRuntimeKeys, ValidatorRuntimeConfig)>,
}

impl NodeRuntimeCapabilities {
    pub fn with_validator(
        mut self,
        keys: ValidatorRuntimeKeys,
        config: ValidatorRuntimeConfig,
    ) -> Self {
        self.validator = Some((keys, config));
        self
    }
}

pub struct NodeRuntime {
    server: QuicServer,
    pub(crate) transport_identity: QuicTransportIdentity,
    backend: NodeStateBackend,
    peer_manager: PeerManager,
    pub(crate) peer_store: PeerStore,
    local_peer_record: Option<PeerRecord>,
    state_recovery_provider: StateRecoveryProviderHandle,
    pub(crate) validator_bft: Option<ValidatorBftRuntime>,
    pub(crate) active_connections: Arc<AtomicUsize>,
    active_submissions: Arc<AtomicUsize>,
}

impl NodeRuntime {
    pub fn load_and_bind(
        listen_address: SocketAddr,
        store: &StateStore,
    ) -> Result<Self, NodeRuntimeError> {
        let persisted = store.load()?.ok_or(NodeRuntimeError::SnapshotMissing)?;
        Self::bind_loaded(
            listen_address,
            store,
            persisted,
            NodeRuntimeCapabilities::default(),
        )
    }

    pub fn bind_loaded(
        listen_address: SocketAddr,
        store: &StateStore,
        persisted: PersistedNodeState,
        capabilities: NodeRuntimeCapabilities,
    ) -> Result<Self, NodeRuntimeError> {
        let recovered_provider = match persisted.recovery_checkpoint_proof.as_ref() {
            Some(proof) => {
                let certified = proof
                    .clone()
                    .verify_checkpoint(&persisted.validator_set)
                    .map_err(|_| NetworkError::InvalidStateRecoveryPayload)?;
                Some(Arc::new(StateRecoveryProvider::new(
                    &persisted.state,
                    &persisted.validator_set,
                    &persisted.validator_registry,
                    &certified,
                )?))
            }
            None => None,
        };
        let validator_bft = match capabilities.validator {
            Some((keys, config)) => Some(ValidatorBftRuntime::new(
                keys,
                config,
                store.clone(),
                persisted.validator_set,
                persisted.retained_validator_sets.into_values(),
            )?),
            None => None,
        };

        Self::bind_backend(
            listen_address,
            NodeStateBackend::Full(store.clone()),
            recovered_provider,
            validator_bft,
        )
    }

    pub fn load_public_and_bind(
        listen_address: SocketAddr,
        store: &PublicStateStore,
    ) -> Result<Self, NodeRuntimeError> {
        let persisted = store.load()?.ok_or(NodeRuntimeError::SnapshotMissing)?;
        Self::bind_public_loaded(listen_address, store, persisted)
    }

    pub fn bind_public_loaded(
        listen_address: SocketAddr,
        store: &PublicStateStore,
        persisted: PersistedPublicNodeState,
    ) -> Result<Self, NodeRuntimeError> {
        persisted
            .validator_registry
            .validate_current_set(&persisted.validator_set)
            .map_err(|_| PersistenceError::ValidatorRegistryMismatch)?;
        Self::bind_backend(
            listen_address,
            NodeStateBackend::Public(store.clone()),
            None,
            None,
        )
    }

    fn bind_backend(
        listen_address: SocketAddr,
        backend: NodeStateBackend,
        recovered_provider: Option<Arc<StateRecoveryProvider>>,
        validator_bft: Option<ValidatorBftRuntime>,
    ) -> Result<Self, NodeRuntimeError> {
        let base_path = backend.base_path();
        let transport_identity =
            QuicTransportIdentity::load_or_generate(transport_identity_path(base_path))?;
        let server = QuicServer::bind(listen_address, &transport_identity)?;
        let local_address = server.local_addr()?;
        let local_peer_record = if local_address.ip().is_unspecified() {
            None
        } else {
            Some(PeerRecord::new(
                transport_identity.node_id(),
                local_address,
                transport_identity.certificate_der().to_vec(),
            )?)
        };
        let peer_manager = PeerManager::new(transport_identity.node_id());
        let peer_store = PeerStore::load(peer_store_path(base_path))?;
        let state_recovery_provider = new_state_recovery_provider_handle();
        *state_recovery_provider
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = recovered_provider;

        Ok(Self {
            server,
            transport_identity,
            backend,
            peer_manager,
            peer_store,
            local_peer_record,
            state_recovery_provider,
            validator_bft,
            active_connections: Arc::new(AtomicUsize::new(0)),
            active_submissions: Arc::new(AtomicUsize::new(0)),
        })
    }

    pub(crate) fn full_store(&self) -> Result<&StateStore, NodeRuntimeError> {
        self.backend
            .full_store()
            .ok_or(NodeRuntimeError::SnapshotMissing)
    }

    pub(crate) fn public_state_store(&self) -> Option<PublicStateStore> {
        match &self.backend {
            NodeStateBackend::Full(_) => None,
            NodeStateBackend::Public(store) => Some(store.clone()),
        }
    }

    pub(crate) fn active_public_peers(&self) -> Vec<QuicPeer> {
        self.peer_manager.peers()
    }

    pub fn local_addr(&self) -> Result<SocketAddr, NodeRuntimeError> {
        self.server.local_addr().map_err(Into::into)
    }

    pub fn node_id(&self) -> NodeId {
        self.transport_identity.node_id()
    }

    pub fn transport_certificate_der(&self) -> &[u8] {
        self.transport_identity.certificate_der()
    }

    pub fn peer(&self, node_id: NodeId) -> Option<QuicPeer> {
        self.peer_manager.peer(node_id)
    }

    pub fn local_peer_record(&self) -> Option<&PeerRecord> {
        self.local_peer_record.as_ref()
    }

    pub async fn sync_freshest_certified_public_currency_view(
        &self,
    ) -> Result<RemoteCertifiedPublicCurrencyView, NodeRuntimeError> {
        let (validator_set, checkpoint_floor_epoch) = match &self.backend {
            NodeStateBackend::Full(store) => {
                let persisted = store.load()?.ok_or(NodeRuntimeError::SnapshotMissing)?;
                (persisted.validator_set, persisted.checkpoint_floor_epoch)
            }
            NodeStateBackend::Public(store) => {
                let persisted = store.load()?.ok_or(NodeRuntimeError::SnapshotMissing)?;
                let floor = persisted
                    .checkpoint_proof
                    .as_ref()
                    .map(|proof| proof.checkpoint().epoch())
                    .unwrap_or(0);
                (persisted.validator_set, floor)
            }
        };

        let peers = self.peer_manager.peers();
        if peers.is_empty() {
            return Err(NodeRuntimeError::NoActivePeers);
        }

        let mut candidates = Vec::new();
        for peer in peers {
            let Ok(Ok(Some(proof))) = tokio::time::timeout(
                PEER_CHECKPOINT_QUERY_TIMEOUT,
                client_public_currency_checkpoint_proof(&peer),
            )
            .await
            else {
                continue;
            };

            let epoch = proof.checkpoint().epoch();
            if epoch < checkpoint_floor_epoch {
                continue;
            }

            let Ok(checkpoint) = proof.verify_checkpoint(&validator_set) else {
                continue;
            };
            candidates.push((epoch, peer.remote_node_id(), peer, checkpoint));
        }

        candidates.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));

        for (_, _, peer, checkpoint) in candidates {
            let sync = client_sync_certified_public_currency_view_from_checkpoint(
                &peer,
                checkpoint,
                &validator_set,
            );
            if let Ok(Ok(synced)) = tokio::time::timeout(PEER_PUBLIC_SYNC_TIMEOUT, sync).await {
                return Ok(synced);
            }
        }

        Err(NodeRuntimeError::NoCertifiedPublicPeer)
    }

    fn known_active_peer_count(&self) -> usize {
        self.peer_store
            .recent(MAX_PEER_RECORDS, &[self.node_id()])
            .into_iter()
            .filter(|record| self.peer_manager.peer(record.node_id()).is_some())
            .count()
    }

    fn public_network_context(&self) -> PublicNetworkContext {
        PublicNetworkContext {
            backend: self.backend.clone(),
            peer_store: self.peer_store.clone(),
            local_node_id: self.node_id(),
            local_peer_record: self.local_peer_record.clone(),
            state_recovery_provider: self.state_recovery_provider.clone(),
        }
    }

    pub fn publish_state_recovery_checkpoint(
        &self,
        checkpoint: CertifiedStateRecoveryCheckpoint,
    ) -> Result<(), NodeRuntimeError> {
        self.full_store()?
            .advance_recovery_checkpoint_floor(&checkpoint)?;
        self.publish_state_recovery_provider(&checkpoint)
    }

    pub(crate) fn publish_state_recovery_provider(
        &self,
        checkpoint: &CertifiedStateRecoveryCheckpoint,
    ) -> Result<(), NodeRuntimeError> {
        let persisted = self
            .full_store()?
            .load()?
            .ok_or(NodeRuntimeError::SnapshotMissing)?;
        let provider = Arc::new(StateRecoveryProvider::new(
            &persisted.state,
            &persisted.validator_set,
            &persisted.validator_registry,
            checkpoint,
        )?);
        *self
            .state_recovery_provider
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(provider);
        Ok(())
    }

    pub async fn bootstrap(
        &self,
        bootstrap_records: &[PeerRecord],
        target_connections: usize,
    ) -> Result<usize, NodeRuntimeError> {
        if target_connections > MAX_ACTIVE_CONNECTIONS {
            return Err(NodeRuntimeError::ConnectionCapacityReached {
                maximum: MAX_ACTIVE_CONNECTIONS,
            });
        }

        let mut candidates = VecDeque::new();
        candidates.extend(self.peer_store.recent(MAX_PEER_RECORDS, &[self.node_id()]));
        candidates.extend(
            bootstrap_records
                .iter()
                .filter(|record| record.node_id() != self.node_id())
                .cloned(),
        );

        let mut attempted = HashSet::new();
        while self.known_active_peer_count() < target_connections {
            let Some(record) = candidates.pop_front() else {
                break;
            };
            if !attempted.insert(record.clone()) {
                continue;
            }

            let peer = if let Some(peer) = self.peer_manager.peer(record.node_id()) {
                peer
            } else {
                match self.dial(&record).await {
                    Ok(peer) => peer,
                    Err(error @ NodeRuntimeError::Network(NetworkError::PeerStore(_))) => {
                        return Err(error);
                    }
                    Err(NodeRuntimeError::ConnectionCapacityReached { .. }) => break,
                    Err(_) => continue,
                }
            };

            if let Ok(records) = client_peer_records(&peer, MAX_PEER_RECORDS).await {
                for record in records {
                    if record.node_id() == peer.remote_node_id() {
                        self.peer_store.record_authenticated(&record)?;
                    } else if record.node_id() != self.node_id() {
                        candidates.push_back(record);
                    }
                }
            }
        }

        Ok(self.known_active_peer_count())
    }

    pub async fn dial(&self, record: &PeerRecord) -> Result<QuicPeer, NodeRuntimeError> {
        let permit = ActiveConnectionPermit::try_acquire(&self.active_connections).ok_or(
            NodeRuntimeError::ConnectionCapacityReached {
                maximum: MAX_ACTIVE_CONNECTIONS,
            },
        )?;

        let client = match QuicClient::new(
            outbound_bind_address(record.address()),
            record.certificate_der(),
            self.transport_identity.clone(),
        ) {
            Ok(client) => client,
            Err(error) => {
                self.peer_store.record_failure(record)?;
                return Err(error.into());
            }
        };
        let peer = match client
            .connect_expected(record.address(), record.node_id())
            .await
        {
            Ok(peer) => peer,
            Err(error) => {
                self.peer_store.record_failure(record)?;
                return Err(error.into());
            }
        };
        let peer_lease = match self.peer_manager.register(&peer, PeerDirection::Outbound) {
            Ok(lease) => lease,
            Err(PeerRegistrationError::Duplicate(node_id)) => {
                peer.close_with_reason(b"duplicate peer");
                return self
                    .peer_manager
                    .peer(node_id)
                    .ok_or(NodeRuntimeError::DuplicatePeer(node_id));
            }
            Err(PeerRegistrationError::SelfConnection) => {
                peer.close_with_reason(b"self connection");
                return Err(NodeRuntimeError::SelfConnection);
            }
        };

        if let Err(error) = self.peer_store.record_authenticated(record) {
            peer.close_with_reason(b"peer store failure");
            drop(peer_lease);
            return Err(error.into());
        }

        let active_peer = peer.clone();
        tokio::spawn(serve_managed_peer(
            peer,
            self.public_network_context(),
            peer_lease,
            permit,
            Some(client),
            None,
        ));

        Ok(active_peer)
    }

    pub async fn run(&self, bootstrap_records: &[PeerRecord]) -> Result<(), NodeRuntimeError> {
        tokio::select! {
            result = self.run_listener() => result,
            result = self.maintain_peers(bootstrap_records) => result,
            result = self.run_validator_bft_consensus() => result,
            result = self.run_public_state_sync() => result,
        }
    }

    async fn run_listener(&self) -> Result<(), NodeRuntimeError> {
        loop {
            let incoming = self.server.accept_incoming().await?;
            let Some(permit) = ActiveConnectionPermit::try_acquire(&self.active_connections) else {
                drop(incoming);
                continue;
            };

            let context = self.public_network_context();
            let peer_manager = self.peer_manager.clone();
            let validator_bft = self.validator_bft.clone();
            let task_submission_context = self.task_submission_context();
            let governance_context = self.governance_context();
            let active_submissions = Arc::clone(&self.active_submissions);
            tokio::spawn(async move {
                let Ok(peer) = incoming.handshake().await else {
                    return;
                };
                let Ok(Some(first_request)) = peer.accept_request().await else {
                    return;
                };

                if matches!(
                    first_request.message(),
                    NetworkMessage::LegalTaskSubmissionOpen { .. }
                ) {
                    let Some(context) = task_submission_context else {
                        let _ = first_request
                            .respond(&NetworkMessage::LegalTaskSubmissionRejected {
                                reason: crate::network::LegalTaskSubmissionRejection::Unavailable,
                            })
                            .await;
                        return;
                    };
                    let Some(_submission_permit) = ActiveConnectionPermit::try_acquire_with_limit(
                        &active_submissions,
                        MAX_ACTIVE_LEGAL_TASK_SUBMISSIONS,
                    ) else {
                        let _ = first_request
                            .respond(&NetworkMessage::LegalTaskSubmissionRejected {
                                reason: crate::network::LegalTaskSubmissionRejection::Busy,
                            })
                            .await;
                        return;
                    };
                    if serve_legal_task_submission_from_request(context, &peer, first_request)
                        .await
                        .is_err()
                    {
                        peer.close_with_reason(b"LegalTask submission failed");
                    }
                    return;
                }

                if matches!(
                    first_request.message(),
                    NetworkMessage::ValidatorTransitionSubmit { .. }
                        | NetworkMessage::StateRecoveryCheckpointSubmit { .. }
                ) {
                    let Some(context) = governance_context else {
                        let _ = first_request
                            .respond(&NetworkMessage::GovernanceRejected {
                                reason: crate::network::GovernanceRejection::Unavailable,
                            })
                            .await;
                        return;
                    };
                    if serve_governance_request_from_request(context, &peer, first_request)
                        .await
                        .is_err()
                    {
                        peer.close_with_reason(b"governance request failed");
                    }
                    return;
                }

                if matches!(
                    first_request.message(),
                    NetworkMessage::BftAuthenticate { .. }
                ) {
                    if let Some(runtime) = validator_bft {
                        if runtime.serve_inbound(&peer, first_request).await.is_err() {
                            peer.close_with_reason(b"validator BFT session failed");
                        }
                    } else {
                        let _ = first_request.respond(&NetworkMessage::BftDenied).await;
                    }
                    return;
                }

                let peer_lease = match peer_manager.register(&peer, PeerDirection::Inbound) {
                    Ok(lease) => lease,
                    Err(PeerRegistrationError::SelfConnection) => {
                        peer.close_with_reason(b"self connection");
                        return;
                    }
                    Err(PeerRegistrationError::Duplicate(_)) => {
                        peer.close_with_reason(b"duplicate peer");
                        return;
                    }
                };

                serve_managed_peer(peer, context, peer_lease, permit, None, Some(first_request))
                    .await;
            });
        }
    }

    async fn maintain_peers(
        &self,
        bootstrap_records: &[PeerRecord],
    ) -> Result<(), NodeRuntimeError> {
        let mut retry_delay = PEER_RETRY_INITIAL_DELAY;
        let mut retry_at = tokio::time::Instant::now();

        loop {
            let active_before = self.known_active_peer_count();
            if active_before >= DEFAULT_ACTIVE_PEER_TARGET {
                retry_delay = PEER_RETRY_INITIAL_DELAY;
                retry_at = tokio::time::Instant::now();
            } else if tokio::time::Instant::now() >= retry_at {
                let active_after = self
                    .bootstrap(bootstrap_records, DEFAULT_ACTIVE_PEER_TARGET)
                    .await?;
                if active_after > active_before {
                    retry_delay = PEER_RETRY_INITIAL_DELAY;
                    retry_at = tokio::time::Instant::now() + retry_delay;
                } else {
                    retry_at = tokio::time::Instant::now() + retry_delay;
                    retry_delay = retry_delay
                        .checked_mul(2)
                        .unwrap_or(PEER_RETRY_MAX_DELAY)
                        .min(PEER_RETRY_MAX_DELAY);
                }
            }

            self.maintain_validator_bft_peers(bootstrap_records).await?;
            tokio::time::sleep(PEER_MAINTENANCE_INTERVAL).await;
        }
    }
}

async fn serve_managed_peer(
    peer: QuicPeer,
    context: PublicNetworkContext,
    peer_lease: PeerLease,
    permit: ActiveConnectionPermit,
    outbound_client: Option<QuicClient>,
    first_request: Option<QuicRequestStream>,
) {
    let _peer_lease = peer_lease;
    let _permit = permit;
    let _outbound_client = outbound_client;

    let refresh_peer = peer.clone();
    let refresh_store = context.peer_store.clone();
    tokio::spawn(async move {
        if let Ok(records) = client_peer_records(&refresh_peer, 1).await
            && let Some(record) = records
                .into_iter()
                .find(|record| record.node_id() == refresh_peer.remote_node_id())
            && refresh_store.record_authenticated(&record).is_err()
        {
            refresh_peer.close_with_reason(b"peer store failure");
        }
    });

    let load_public_snapshot = || match &context.backend {
        NodeStateBackend::Full(store) => {
            let persisted = store
                .load()
                .map_err(|error| NetworkError::PublicStateSource(format!("{error:?}")))?
                .ok_or_else(|| NetworkError::PublicStateSource("snapshot missing".to_owned()))?;
            let view = crate::PublicCurrencyView::new(
                persisted.state.public_currency_summary(),
                persisted.state.public_currency_states(),
            )
            .map_err(NetworkError::PublicState)?;
            Ok(RuntimePublicSnapshot {
                view,
                checkpoint_proof: persisted.public_checkpoint_proof,
                latest_delta: persisted.latest_public_delta,
            })
        }
        NodeStateBackend::Public(store) => {
            let persisted = store
                .load()
                .map_err(|error| NetworkError::PublicStateSource(format!("{error:?}")))?
                .ok_or_else(|| {
                    NetworkError::PublicStateSource("public snapshot missing".to_owned())
                })?;
            let view = persisted.view.ok_or_else(|| {
                NetworkError::PublicStateSource("public state not synchronized".to_owned())
            })?;
            Ok(RuntimePublicSnapshot {
                view,
                checkpoint_proof: persisted.checkpoint_proof,
                latest_delta: None,
            })
        }
    };
    let load_transition_proof = |current_validator_set_version: u64| match &context.backend {
        NodeStateBackend::Full(store) => {
            let persisted = store
                .load()
                .map_err(|error| NetworkError::PublicStateSource(format!("{error:?}")))?
                .ok_or_else(|| NetworkError::PublicStateSource("snapshot missing".to_owned()))?;
            Ok(persisted
                .validator_transition_proofs
                .get(&current_validator_set_version)
                .cloned())
        }
        NodeStateBackend::Public(_) => Ok(None),
    };
    let load_recovery_snapshot = || {
        let store = context.backend.full_store().ok_or_else(|| {
            NetworkError::PublicStateSource(
                "state recovery unavailable on public-only node".to_owned(),
            )
        })?;
        let persisted = store
            .load()
            .map_err(|error| NetworkError::PublicStateSource(format!("{error:?}")))?
            .ok_or_else(|| NetworkError::PublicStateSource("snapshot missing".to_owned()))?;
        Ok(RuntimeNetworkSnapshot {
            state: persisted.state,
            validator_set: persisted.validator_set,
            validator_registry: persisted.validator_registry,
        })
    };
    let recovery_loader = context.backend.full_store().map(|_| {
        &load_recovery_snapshot
            as &(dyn Fn() -> Result<RuntimeNetworkSnapshot, NetworkError> + Send + Sync)
    });
    let services = PublicNetworkServices::new(
        &context.peer_store,
        context.local_node_id,
        context.local_peer_record.as_ref(),
        &context.state_recovery_provider,
        &load_public_snapshot,
        &load_transition_proof,
        recovery_loader,
    );
    let result = match first_request {
        Some(first_request) => {
            serve_public_network_connection_from_request(&peer, services, first_request).await
        }
        None => serve_public_network_connection(&peer, services).await,
    };
    let _ = result;
}

pub(crate) struct ActiveConnectionPermit {
    active_connections: Arc<AtomicUsize>,
}

impl ActiveConnectionPermit {
    pub(crate) fn try_acquire(active_connections: &Arc<AtomicUsize>) -> Option<Self> {
        Self::try_acquire_with_limit(active_connections, MAX_ACTIVE_CONNECTIONS)
    }

    pub(crate) fn try_acquire_with_limit(
        active_connections: &Arc<AtomicUsize>,
        maximum: usize,
    ) -> Option<Self> {
        active_connections
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < maximum).then_some(active + 1)
            })
            .ok()?;

        Some(Self {
            active_connections: Arc::clone(active_connections),
        })
    }
}

impl Drop for ActiveConnectionPermit {
    fn drop(&mut self) {
        self.active_connections.fetch_sub(1, Ordering::AcqRel);
    }
}

fn peer_store_path(base_path: &Path) -> PathBuf {
    let mut path = OsString::from(base_path.as_os_str());
    path.push(".peers");
    PathBuf::from(path)
}
