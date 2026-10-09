use std::collections::{HashSet, VecDeque};
use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

mod peer_connectivity;
mod public_serving;
#[cfg(test)]
mod scheduling_tests;
use public_serving::serve_managed_peer;

pub(crate) const MAX_ACTIVE_CONNECTIONS: usize = 128;
const MAX_ACTIVE_LEGAL_TASK_SUBMISSIONS: usize = 8;
pub const DEFAULT_ACTIVE_PEER_TARGET: usize = 8;
pub(crate) const PEER_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(2);
const PEER_RETRY_INITIAL_DELAY: Duration = Duration::from_secs(1);
const PEER_RETRY_MAX_DELAY: Duration = Duration::from_secs(60);

type PublicViewCache = Arc<std::sync::Mutex<Option<(u64, Arc<crate::PublicCurrencyView>)>>>;

use crate::network::{
    MAX_LOCAL_PEER_CANDIDATES, MAX_PEER_RECORDS, NetworkError, NetworkMessage, NodeId,
    PeerDirection, PeerLease, PeerManager, PeerRecord, PeerRegistrationError, PeerStore,
    PublicNetworkServices, QuicClient, QuicPeer, QuicRequestStream, QuicServer,
    QuicTransportIdentity, RuntimeNetworkSnapshot, RuntimePublicSnapshot, StateRecoveryProvider,
    StateRecoveryProviderHandle, client_peer_records, new_state_recovery_provider_handle,
    outbound_bind_address, serve_public_network_connection,
    serve_public_network_connection_from_request, transport_identity_path,
};
use crate::runtime_bft::{
    ValidatorBftRuntime, ValidatorBftRuntimeError, ValidatorRuntimeConfig, ValidatorRuntimeKeys,
    load_transition_proof,
};
use crate::runtime_governance::serve_governance_request_from_request;
use crate::runtime_submission::serve_legal_task_submission_from_request;
use crate::runtime_task_status::serve_legal_task_status_from_request;
use crate::{
    AuthorizationError, BftConsensusRuntimeError, BftDriverError, PersistedNodeState,
    PersistedPublicNodeState, PersistenceError, PreparationError, PublicStateStore, StateStore,
    TaskEncodingError,
};

#[derive(Debug)]
pub enum NodeRuntimeError {
    RuntimeTaskFailed(tokio::task::JoinError),
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
    local_peer_record: Option<Arc<PeerRecord>>,
    state_recovery_provider: StateRecoveryProviderHandle,
    public_view_cache: PublicViewCache,
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
    pub(crate) server: QuicServer,
    pub(crate) transport_identity: QuicTransportIdentity,
    backend: NodeStateBackend,
    peer_manager: PeerManager,
    pub(crate) peer_store: PeerStore,
    local_peer_record: Option<PeerRecord>,
    state_recovery_provider: StateRecoveryProviderHandle,
    pub(crate) validator_bft: Option<ValidatorBftRuntime>,
    pub(crate) active_connections: Arc<AtomicUsize>,
    active_submissions: Arc<AtomicUsize>,
    public_view_cache: PublicViewCache,
}

impl Drop for NodeRuntime {
    fn drop(&mut self) {
        self.server.close();
    }
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
                    &persisted, &certified,
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
            public_view_cache: Arc::new(std::sync::Mutex::new(None)),
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

    fn public_network_context(&self) -> PublicNetworkContext {
        PublicNetworkContext {
            backend: self.backend.clone(),
            peer_store: self.peer_store.clone(),
            local_node_id: self.node_id(),
            local_peer_record: self.local_peer_record.clone().map(Arc::new),
            state_recovery_provider: self.state_recovery_provider.clone(),
            public_view_cache: Arc::clone(&self.public_view_cache),
        }
    }

    pub(crate) fn state_recovery_provider_handle(&self) -> &StateRecoveryProviderHandle {
        &self.state_recovery_provider
    }

    pub async fn run(
        self: &Arc<Self>,
        bootstrap_records: &[PeerRecord],
    ) -> Result<(), NodeRuntimeError> {
        tokio::select! {
            result = self.run_listener() => result,
            result = self.maintain_peers(bootstrap_records) => result,
            result = self.run_validator_bft_consensus() => result,
            result = self.run_public_state_sync() => result,
        }
    }

    pub(crate) async fn run_listener(&self) -> Result<(), NodeRuntimeError> {
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
            let task_status_context = self.task_status_context();
            let governance_context = self.governance_context();
            let active_submissions = Arc::clone(&self.active_submissions);
            tokio::spawn(async move {
                let Ok(peer) = incoming.handshake().await else {
                    return;
                };
                let Ok(Ok(Some(first_request))) =
                    tokio::time::timeout(Duration::from_secs(5), peer.accept_request()).await
                else {
                    peer.close_with_reason(b"first request timed out");
                    return;
                };

                if matches!(
                    first_request.message(),
                    NetworkMessage::LegalTaskStatusQuery { .. }
                ) {
                    let Some(context) = task_status_context else {
                        let _ = first_request
                            .respond(&NetworkMessage::LegalTaskStatusRejected {
                                reason: crate::network::LegalTaskStatusRejection::Unavailable,
                            })
                            .await;
                        return;
                    };
                    if serve_legal_task_status_from_request(context, first_request, permit)
                        .await
                        .is_err()
                    {
                        peer.close_with_reason(b"LegalTask status query failed");
                    }
                    return;
                }

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
                    let Some(submission_permit) = ActiveConnectionPermit::try_acquire_with_limit(
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
                    if !matches!(
                        tokio::time::timeout(
                            Duration::from_secs(30),
                            serve_legal_task_submission_from_request(
                                context,
                                &peer,
                                first_request,
                                submission_permit,
                            )
                        )
                        .await,
                        Ok(Ok(()))
                    ) {
                        peer.close_with_reason(b"LegalTask submission failed");
                    }
                    return;
                }

                if matches!(
                    first_request.message(),
                    NetworkMessage::ValidatorTransitionSubmit { .. }
                        | NetworkMessage::PublicCheckpointSubmit { .. }
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
                    if serve_governance_request_from_request(context, &peer, first_request, permit)
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

                if matches!(
                    first_request.message(),
                    NetworkMessage::GetValidatorSetTransitionProof { .. }
                ) {
                    let holder = Arc::new((context, permit));
                    let loader = Arc::new(move |version| {
                        load_transition_proof(holder.0.backend.full_store(), version)
                    });
                    let result = tokio::time::timeout(
                        Duration::from_secs(5),
                        crate::network::serve_transition_proof_requests(
                            &peer,
                            first_request,
                            loader,
                        ),
                    )
                    .await;
                    if !matches!(result, Ok(Ok(()))) {
                        peer.close_with_reason(b"membership proof query failed");
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
