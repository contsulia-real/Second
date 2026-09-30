use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const MAX_ACTIVE_CONNECTIONS: usize = 128;

use crate::{
    NetworkError, NodeId, PersistenceError, PublicCurrencyCheckpointProof, QuicServer,
    QuicTransportIdentity, SecondState, StateStore, serve_public_currency_connection,
};

#[derive(Debug)]
pub enum NodeRuntimeError {
    Persistence(PersistenceError),
    Network(NetworkError),
    SnapshotMissing,
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

        Ok(Self {
            server,
            transport_identity,
            state: Arc::new(persisted.state),
            public_checkpoint_proof: persisted.public_checkpoint_proof.map(Arc::new),
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

    pub async fn run(self) -> Result<(), NodeRuntimeError> {
        let active_connections = Arc::new(AtomicUsize::new(0));

        loop {
            let incoming = self.server.accept_incoming().await?;
            let Some(permit) = ActiveConnectionPermit::try_acquire(&active_connections) else {
                drop(incoming);
                continue;
            };

            let state = Arc::clone(&self.state);
            let public_checkpoint_proof = self.public_checkpoint_proof.clone();
            tokio::spawn(async move {
                let _permit = permit;
                let Ok(peer) = incoming.handshake().await else {
                    return;
                };

                let _ = serve_public_currency_connection(
                    &peer,
                    &state,
                    public_checkpoint_proof.as_deref(),
                )
                .await;
            });
        }
    }
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
