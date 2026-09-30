use std::ffi::OsString;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const MAX_ACTIVE_CONNECTIONS: usize = 128;

use crate::network::{PeerDirection, PeerLease, PeerManager, PeerRegistrationError};
use crate::{
    NetworkError, NodeId, PersistenceError, PublicCurrencyCheckpointProof, QuicClient, QuicPeer,
    QuicServer, QuicTransportIdentity, SecondState, StateStore, serve_public_currency_connection,
};

#[derive(Debug)]
pub enum NodeRuntimeError {
    Persistence(PersistenceError),
    Network(NetworkError),
    SnapshotMissing,
    ConnectionCapacityReached { maximum: usize },
    SelfConnection,
    DuplicatePeer(NodeId),
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

pub struct NodeRuntime {
    server: QuicServer,
    transport_identity: QuicTransportIdentity,
    state: Arc<SecondState>,
    public_checkpoint_proof: Option<Arc<PublicCurrencyCheckpointProof>>,
    peer_manager: PeerManager,
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
        let peer_manager = PeerManager::new(transport_identity.node_id());

        Ok(Self {
            server,
            transport_identity,
            state: Arc::new(persisted.state),
            public_checkpoint_proof: persisted.public_checkpoint_proof.map(Arc::new),
            peer_manager,
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

    pub async fn dial(
        &self,
        address: SocketAddr,
        expected_node_id: NodeId,
        trusted_server_certificate_der: &[u8],
    ) -> Result<QuicPeer, NodeRuntimeError> {
        let permit = ActiveConnectionPermit::try_acquire(&self.active_connections).ok_or(
            NodeRuntimeError::ConnectionCapacityReached {
                maximum: MAX_ACTIVE_CONNECTIONS,
            },
        )?;

        let client = QuicClient::new(
            outbound_bind_address(address),
            trusted_server_certificate_der,
            self.transport_identity.clone(),
        )?;
        let peer = client.connect_expected(address, expected_node_id).await?;
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

        let active_peer = peer.clone();
        tokio::spawn(serve_managed_peer(
            peer,
            Arc::clone(&self.state),
            self.public_checkpoint_proof.clone(),
            peer_lease,
            permit,
            Some(client),
        ));

        Ok(active_peer)
    }

    pub async fn run(&self) -> Result<(), NodeRuntimeError> {
        loop {
            let incoming = self.server.accept_incoming().await?;
            let Some(permit) = ActiveConnectionPermit::try_acquire(&self.active_connections) else {
                drop(incoming);
                continue;
            };

            let state = Arc::clone(&self.state);
            let public_checkpoint_proof = self.public_checkpoint_proof.clone();
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

                serve_managed_peer(
                    peer,
                    state,
                    public_checkpoint_proof,
                    peer_lease,
                    permit,
                    None,
                )
                .await;
            });
        }
    }
}

async fn serve_managed_peer(
    peer: QuicPeer,
    state: Arc<SecondState>,
    public_checkpoint_proof: Option<Arc<PublicCurrencyCheckpointProof>>,
    peer_lease: PeerLease,
    permit: ActiveConnectionPermit,
    outbound_client: Option<QuicClient>,
) {
    let _peer_lease = peer_lease;
    let _permit = permit;
    let _outbound_client = outbound_client;
    let _ =
        serve_public_currency_connection(&peer, &state, public_checkpoint_proof.as_deref()).await;
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

fn transport_identity_path(store: &StateStore) -> PathBuf {
    let mut path = OsString::from(store.base_path().as_os_str());
    path.push(".transport");
    PathBuf::from(path)
}
