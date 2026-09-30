use std::net::SocketAddr;
use std::sync::Arc;

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
    local_node_id: NodeId,
    state: Arc<SecondState>,
    public_checkpoint_proof: Option<Arc<PublicCurrencyCheckpointProof>>,
}

impl NodeRuntime {
    pub fn load_and_bind(
        listen_address: SocketAddr,
        local_node_id: NodeId,
        store: &StateStore,
    ) -> Result<Self, NodeRuntimeError> {
        let persisted = store.load()?.ok_or(NodeRuntimeError::SnapshotMissing)?;
        let transport_identity = QuicTransportIdentity::generate()?;
        let server = QuicServer::bind(listen_address, &transport_identity)?;

        Ok(Self {
            server,
            transport_identity,
            local_node_id,
            state: Arc::new(persisted.state),
            public_checkpoint_proof: persisted.public_checkpoint_proof.map(Arc::new),
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr, NodeRuntimeError> {
        self.server.local_addr().map_err(Into::into)
    }

    pub fn transport_certificate_der(&self) -> &[u8] {
        self.transport_identity.certificate_der()
    }

    pub async fn run(self) -> Result<(), NodeRuntimeError> {
        loop {
            let incoming = self.server.accept_incoming().await?;
            let state = Arc::clone(&self.state);
            let public_checkpoint_proof = self.public_checkpoint_proof.clone();
            let local_node_id = self.local_node_id;

            tokio::spawn(async move {
                let Ok(peer) = incoming.handshake(local_node_id).await else {
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
