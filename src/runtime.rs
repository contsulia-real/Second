use std::collections::{HashSet, VecDeque};
use std::ffi::OsString;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

const MAX_ACTIVE_CONNECTIONS: usize = 128;
pub const DEFAULT_ACTIVE_PEER_TARGET: usize = 8;
const PEER_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(2);
const PEER_RETRY_INITIAL_DELAY: Duration = Duration::from_secs(1);
const PEER_RETRY_MAX_DELAY: Duration = Duration::from_secs(60);
const PEER_CHECKPOINT_QUERY_TIMEOUT: Duration = Duration::from_secs(5);
const PEER_PUBLIC_SYNC_TIMEOUT: Duration = Duration::from_secs(30);

use crate::network::{
    MAX_PEER_RECORDS, NetworkError, NodeId, PeerDirection, PeerLease, PeerManager, PeerRecord,
    PeerRegistrationError, PeerStore, QuicClient, QuicPeer, QuicServer, QuicTransportIdentity,
    StateRecoveryProvider, StateRecoveryProviderHandle, client_peer_records,
    client_public_currency_checkpoint_proof,
    client_sync_certified_public_currency_view_from_checkpoint, new_state_recovery_provider_handle,
    serve_public_network_connection,
};
use crate::{
    CertifiedStateRecoveryCheckpoint, PersistenceError, PublicCurrencyCheckpointProof,
    RemoteCertifiedPublicCurrencyView, SecondState, StateStore, ValidatorRegistry, ValidatorSet,
};

#[derive(Debug)]
pub enum NodeRuntimeError {
    Persistence(PersistenceError),
    Network(NetworkError),
    SnapshotMissing,
    ConnectionCapacityReached { maximum: usize },
    SelfConnection,
    DuplicatePeer(NodeId),
    NoActivePeers,
    NoCertifiedPublicPeer,
}

impl From<PersistenceError> for NodeRuntimeError {
    fn from(error: PersistenceError) -> Self {
        Self::Persistence(error)
    }
}

impl From<NetworkError> for NodeRuntimeError {
    fn from(error: NetworkError) -> Self {
        Self::Network(error)
    }
}

#[derive(Clone)]
struct PublicNetworkContext {
    state: Arc<SecondState>,
    public_checkpoint_proof: Option<Arc<PublicCurrencyCheckpointProof>>,
    peer_store: PeerStore,
    local_node_id: NodeId,
    local_peer_record: Option<PeerRecord>,
    state_recovery_provider: StateRecoveryProviderHandle,
}

pub struct NodeRuntime {
    server: QuicServer,
    transport_identity: QuicTransportIdentity,
    state: Arc<SecondState>,
    validator_set: ValidatorSet,
    validator_registry: ValidatorRegistry,
    checkpoint_floor_epoch: u64,
    public_checkpoint_proof: Option<Arc<PublicCurrencyCheckpointProof>>,
    peer_manager: PeerManager,
    peer_store: PeerStore,
    local_peer_record: Option<PeerRecord>,
    state_recovery_provider: StateRecoveryProviderHandle,
    active_connections: Arc<AtomicUsize>,
}

impl NodeRuntime {
    pub fn load_and_bind(
        listen_address: SocketAddr,
        store: &StateStore,
    ) -> Result<Self, NodeRuntimeError> {
        let persisted = store.load()?.ok_or(NodeRuntimeError::SnapshotMissing)?;
        let transport_identity =
            QuicTransportIdentity::load_or_generate(transport_identity_path(store))?;
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
        let peer_store = PeerStore::load(peer_store_path(store))?;
        let validator_set = persisted.validator_set.clone();
        let validator_registry = persisted.validator_registry.clone();
        let checkpoint_floor_epoch = persisted.checkpoint_floor_epoch;

        Ok(Self {
            server,
            transport_identity,
            state: Arc::new(persisted.state),
            validator_set,
            validator_registry,
            checkpoint_floor_epoch,
            public_checkpoint_proof: persisted.public_checkpoint_proof.map(Arc::new),
            peer_manager,
            peer_store,
            local_peer_record,
            state_recovery_provider: new_state_recovery_provider_handle(),
            active_connections: Arc::new(AtomicUsize::new(0)),
        })
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
            if epoch < self.checkpoint_floor_epoch {
                continue;
            }

            let Ok(checkpoint) = proof.verify_checkpoint(&self.validator_set) else {
                continue;
            };
            candidates.push((epoch, peer.remote_node_id(), peer, checkpoint));
        }

        candidates.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));

        for (_, _, peer, checkpoint) in candidates {
            let sync = client_sync_certified_public_currency_view_from_checkpoint(
                &peer,
                checkpoint,
                &self.validator_set,
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
            state: Arc::clone(&self.state),
            public_checkpoint_proof: self.public_checkpoint_proof.clone(),
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
        let provider = Arc::new(StateRecoveryProvider::new(
            &self.state,
            &self.validator_set,
            &self.validator_registry,
            &checkpoint,
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
        ));

        Ok(active_peer)
    }

    pub async fn run(&self, bootstrap_records: &[PeerRecord]) -> Result<(), NodeRuntimeError> {
        tokio::select! {
            result = self.run_listener() => result,
            result = self.maintain_peers(bootstrap_records) => result,
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
            tokio::spawn(async move {
                let Ok(peer) = incoming.handshake().await else {
                    return;
                };

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

                serve_managed_peer(peer, context, peer_lease, permit, None).await;
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

    let _ = serve_public_network_connection(
        &peer,
        &context.state,
        context.public_checkpoint_proof.as_deref(),
        &context.peer_store,
        context.local_node_id,
        context.local_peer_record.as_ref(),
        &context.state_recovery_provider,
    )
    .await;
}

fn outbound_bind_address(remote: SocketAddr) -> SocketAddr {
    let ip = match remote {
        SocketAddr::V4(_) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        SocketAddr::V6(_) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
    };
    SocketAddr::new(ip, 0)
}

struct ActiveConnectionPermit {
    active_connections: Arc<AtomicUsize>,
}

impl ActiveConnectionPermit {
    fn try_acquire(active_connections: &Arc<AtomicUsize>) -> Option<Self> {
        active_connections
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < MAX_ACTIVE_CONNECTIONS).then_some(active + 1)
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

fn peer_store_path(store: &StateStore) -> PathBuf {
    let mut path = OsString::from(store.base_path().as_os_str());
    path.push(".peers");
    PathBuf::from(path)
}

fn transport_identity_path(store: &StateStore) -> PathBuf {
    let mut path = OsString::from(store.base_path().as_os_str());
    path.push(".transport");
    PathBuf::from(path)
}
