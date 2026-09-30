use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use quinn::{Connection, Endpoint, RecvStream, SendStream, TransportConfig};
use rcgen::CertifiedKey;
use rustls::RootCertStore;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

use super::codec::{MAX_NETWORK_MESSAGE_SIZE, decode_network_message, encode_network_message};
use super::{NetworkError, NetworkMessage, NodeId};
const QUIC_IDLE_TIMEOUT: Duration = Duration::from_secs(5);

pub const SECOND_QUIC_SERVER_NAME: &str = "second.local";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuicTransportIdentity {
    certificate_der: Vec<u8>,
    private_key_der: Vec<u8>,
}

impl QuicTransportIdentity {
    pub fn generate() -> Result<Self, NetworkError> {
        let CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec![SECOND_QUIC_SERVER_NAME.to_owned()])
                .map_err(transport_error)?;

        Ok(Self {
            certificate_der: cert.der().to_vec(),
            private_key_der: signing_key.serialize_der(),
        })
    }

    pub fn certificate_der(&self) -> &[u8] {
        &self.certificate_der
    }
}

#[derive(Debug)]
pub struct QuicServer {
    endpoint: Endpoint,
}

impl QuicServer {
    pub fn bind(
        address: SocketAddr,
        identity: &QuicTransportIdentity,
    ) -> Result<Self, NetworkError> {
        let certificate = CertificateDer::from(identity.certificate_der.clone());
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(identity.private_key_der.clone()));
        let mut config = quinn::ServerConfig::with_single_cert(vec![certificate], key)
            .map_err(transport_error)?;
        config.transport_config(transport_config());

        let endpoint = Endpoint::server(config, address).map_err(transport_error)?;
        Ok(Self { endpoint })
    }

    pub fn local_addr(&self) -> Result<SocketAddr, NetworkError> {
        self.endpoint.local_addr().map_err(transport_error)
    }

    pub(crate) async fn accept_incoming(&self) -> Result<QuicIncoming, NetworkError> {
        let incoming = self
            .endpoint
            .accept()
            .await
            .ok_or_else(|| NetworkError::Transport("QUIC endpoint is closed".to_owned()))?;
        Ok(QuicIncoming { incoming })
    }

    pub async fn accept(&self, local_node_id: NodeId) -> Result<QuicPeer, NetworkError> {
        self.accept_incoming().await?.handshake(local_node_id).await
    }
}

pub(crate) struct QuicIncoming {
    incoming: quinn::Incoming,
}

impl QuicIncoming {
    pub(crate) async fn handshake(self, local_node_id: NodeId) -> Result<QuicPeer, NetworkError> {
        let connection = self.incoming.await.map_err(transport_error)?;
        server_handshake(connection, local_node_id).await
    }
}

#[derive(Debug)]
pub struct QuicClient {
    endpoint: Endpoint,
}

impl QuicClient {
    pub fn new(
        bind_address: SocketAddr,
        trusted_server_certificate_der: &[u8],
    ) -> Result<Self, NetworkError> {
        let mut roots = RootCertStore::empty();
        roots
            .add(CertificateDer::from(
                trusted_server_certificate_der.to_vec(),
            ))
            .map_err(transport_error)?;

        let mut config = quinn::ClientConfig::with_root_certificates(Arc::new(roots))
            .map_err(transport_error)?;
        config.transport_config(transport_config());

        let mut endpoint = Endpoint::client(bind_address).map_err(transport_error)?;
        endpoint.set_default_client_config(config);

        Ok(Self { endpoint })
    }

    pub async fn connect(
        &self,
        address: SocketAddr,
        local_node_id: NodeId,
    ) -> Result<QuicPeer, NetworkError> {
        let connecting = self
            .endpoint
            .connect(address, SECOND_QUIC_SERVER_NAME)
            .map_err(transport_error)?;
        let connection = connecting.await.map_err(transport_error)?;
        client_handshake(connection, local_node_id).await
    }

    pub async fn wait_idle(&self) {
        self.endpoint.wait_idle().await;
    }
}

#[derive(Clone, Debug)]
pub struct QuicPeer {
    connection: Connection,
    remote_node_id: NodeId,
}

impl QuicPeer {
    pub const fn remote_node_id(&self) -> NodeId {
        self.remote_node_id
    }

    pub fn close(&self) {
        self.connection.close(0_u32.into(), b"done");
    }

    pub async fn exchange(&self, message: &NetworkMessage) -> Result<NetworkMessage, NetworkError> {
        let (mut send, mut recv) = self.connection.open_bi().await.map_err(transport_error)?;
        write_stream_message(&mut send, message).await?;
        read_stream_message(&mut recv).await
    }

    pub async fn accept_request(&self) -> Result<Option<QuicRequestStream>, NetworkError> {
        match self.connection.accept_bi().await {
            Ok((send, mut recv)) => {
                let message = read_stream_message(&mut recv).await?;
                Ok(Some(QuicRequestStream { send, message }))
            }
            Err(quinn::ConnectionError::ApplicationClosed(_))
            | Err(quinn::ConnectionError::LocallyClosed) => Ok(None),
            Err(error) => Err(transport_error(error)),
        }
    }
}

pub struct QuicRequestStream {
    send: SendStream,
    message: NetworkMessage,
}

impl QuicRequestStream {
    pub fn message(&self) -> &NetworkMessage {
        &self.message
    }

    pub async fn respond(mut self, response: &NetworkMessage) -> Result<(), NetworkError> {
        write_stream_message(&mut self.send, response).await
    }
}

fn transport_config() -> Arc<TransportConfig> {
    let mut config = TransportConfig::default();
    config.max_concurrent_bidi_streams(1_u8.into());
    config.max_concurrent_uni_streams(0_u8.into());
    config.max_idle_timeout(Some(
        QUIC_IDLE_TIMEOUT
            .try_into()
            .expect("five seconds is a valid QUIC idle timeout"),
    ));
    Arc::new(config)
}

async fn client_handshake(
    connection: Connection,
    local_node_id: NodeId,
) -> Result<QuicPeer, NetworkError> {
    let (mut send, mut recv) = connection.open_bi().await.map_err(transport_error)?;
    write_stream_message(
        &mut send,
        &NetworkMessage::Hello {
            node_id: local_node_id,
        },
    )
    .await?;

    let remote_node_id = match read_stream_message(&mut recv).await? {
        NetworkMessage::Hello { node_id } => node_id,
        _ => return Err(NetworkError::UnexpectedMessage),
    };

    Ok(QuicPeer {
        connection,
        remote_node_id,
    })
}

async fn server_handshake(
    connection: Connection,
    local_node_id: NodeId,
) -> Result<QuicPeer, NetworkError> {
    let (mut send, mut recv) = connection.accept_bi().await.map_err(transport_error)?;

    let remote_node_id = match read_stream_message(&mut recv).await? {
        NetworkMessage::Hello { node_id } => node_id,
        _ => return Err(NetworkError::UnexpectedMessage),
    };

    write_stream_message(
        &mut send,
        &NetworkMessage::Hello {
            node_id: local_node_id,
        },
    )
    .await?;

    Ok(QuicPeer {
        connection,
        remote_node_id,
    })
}

async fn write_stream_message(
    send: &mut SendStream,
    message: &NetworkMessage,
) -> Result<(), NetworkError> {
    let frame = encode_network_message(message)?;
    send.write_all(&frame).await.map_err(transport_error)?;
    send.finish().map_err(transport_error)?;

    match send.stopped().await.map_err(transport_error)? {
        None => Ok(()),
        Some(code) => Err(NetworkError::Transport(format!(
            "QUIC stream stopped by peer with code {code}"
        ))),
    }
}

async fn read_stream_message(recv: &mut RecvStream) -> Result<NetworkMessage, NetworkError> {
    let frame = recv
        .read_to_end(MAX_NETWORK_MESSAGE_SIZE)
        .await
        .map_err(transport_error)?;
    decode_network_message(&frame)
}

fn transport_error(error: impl std::fmt::Display) -> NetworkError {
    NetworkError::Transport(error.to_string())
}
