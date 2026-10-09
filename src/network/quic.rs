use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

use std::time::Duration;

use quinn::{Connection, Endpoint, RecvStream, SendStream, TransportConfig};
use rustls::RootCertStore;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

use super::codec::{MAX_NETWORK_MESSAGE_SIZE, decode_network_message, encode_network_message};
use super::identity::{PeerAuthRole, QuicTransportIdentity};
use super::{NetworkError, NetworkMessage, NodeId};

const QUIC_IDLE_TIMEOUT: Duration = Duration::from_secs(5);
const QUIC_OUTBOUND_KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(2);
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

async fn request_deadline<T>(
    stage: &'static str,
    future: impl std::future::Future<Output = Result<T, NetworkError>>,
) -> Result<T, NetworkError> {
    request_deadline_at(tokio::time::Instant::now() + REQUEST_TIMEOUT, stage, future).await
}

async fn request_deadline_at<T>(
    deadline: tokio::time::Instant,
    stage: &'static str,
    future: impl std::future::Future<Output = Result<T, NetworkError>>,
) -> Result<T, NetworkError> {
    tokio::time::timeout_at(deadline, future)
        .await
        .map_err(|_| NetworkError::Transport(format!("protocol request timed out while {stage}")))?
}
pub(crate) const MAX_CONCURRENT_REQUEST_STREAMS: usize = 8;
pub(crate) const MAX_CONCURRENT_ONE_WAY_STREAMS: usize = 8;
const PEER_AUTH_EXPORTER_LABEL: &[u8] = b"SECOND_QUIC_PEER_AUTH_V1";

pub const SECOND_QUIC_SERVER_NAME: &str = "second.local";

#[derive(Clone)]
pub struct QuicServer {
    endpoint: Endpoint,
    identity: Arc<QuicTransportIdentity>,
}

impl QuicServer {
    pub fn bind(
        address: SocketAddr,
        identity: &QuicTransportIdentity,
    ) -> Result<Self, NetworkError> {
        let certificate = CertificateDer::from(identity.certificate_der().to_vec());
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
            identity.private_key_der().to_vec(),
        ));
        let mut config = quinn::ServerConfig::with_single_cert(vec![certificate], key)
            .map_err(transport_error)?;
        config.transport_config(transport_config());

        let endpoint = Endpoint::server(config, address).map_err(transport_error)?;
        Ok(Self {
            endpoint,
            identity: Arc::new(identity.clone()),
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr, NetworkError> {
        self.endpoint.local_addr().map_err(transport_error)
    }

    pub fn node_id(&self) -> NodeId {
        self.identity.node_id()
    }

    pub(crate) fn close(&self) {
        self.endpoint.close(0_u32.into(), b"node stopped");
    }

    pub(crate) fn shared_client(
        &self,
        remote: SocketAddr,
        certificate: &[u8],
    ) -> Result<Option<QuicClient>, NetworkError> {
        if self.local_addr()?.is_ipv4() != remote.is_ipv4() {
            return Ok(None);
        }
        // The socket/driver is shared; each handle keeps its own certificate pin.
        let mut endpoint = self.endpoint.clone();
        endpoint.set_default_client_config(pinned_client_config(certificate)?);
        Ok(Some(QuicClient {
            endpoint,
            identity: Arc::clone(&self.identity),
        }))
    }

    pub(crate) async fn accept_incoming(&self) -> Result<QuicIncoming, NetworkError> {
        let incoming = self
            .endpoint
            .accept()
            .await
            .ok_or_else(|| NetworkError::Transport("QUIC endpoint is closed".to_owned()))?;
        Ok(QuicIncoming {
            incoming,
            identity: Arc::clone(&self.identity),
        })
    }

    pub async fn accept(&self) -> Result<QuicPeer, NetworkError> {
        self.accept_incoming().await?.handshake().await
    }
}

pub(crate) struct QuicIncoming {
    incoming: quinn::Incoming,
    identity: Arc<QuicTransportIdentity>,
}

impl QuicIncoming {
    pub(crate) async fn handshake(self) -> Result<QuicPeer, NetworkError> {
        request_deadline("authenticating incoming connection", async {
            let connection = self.incoming.await.map_err(transport_error)?;
            server_handshake(connection, &self.identity).await
        })
        .await
    }
}

pub struct QuicClient {
    endpoint: Endpoint,
    identity: Arc<QuicTransportIdentity>,
}

impl QuicClient {
    pub fn new(
        bind_address: SocketAddr,
        trusted_server_certificate_der: &[u8],
        identity: QuicTransportIdentity,
    ) -> Result<Self, NetworkError> {
        let config = pinned_client_config(trusted_server_certificate_der)?;

        let mut endpoint = Endpoint::client(bind_address).map_err(transport_error)?;
        endpoint.set_default_client_config(config);

        Ok(Self {
            endpoint,
            identity: Arc::new(identity),
        })
    }

    pub fn node_id(&self) -> NodeId {
        self.identity.node_id()
    }

    pub async fn connect(&self, address: SocketAddr) -> Result<QuicPeer, NetworkError> {
        let connecting = self
            .endpoint
            .connect(address, SECOND_QUIC_SERVER_NAME)
            .map_err(transport_error)?;
        request_deadline("connecting and authenticating peer", async {
            let connection = connecting.await.map_err(transport_error)?;
            client_handshake(connection, &self.identity).await
        })
        .await
    }

    pub async fn connect_expected(
        &self,
        address: SocketAddr,
        expected_node_id: NodeId,
    ) -> Result<QuicPeer, NetworkError> {
        let peer = self.connect(address).await?;
        let actual = peer.remote_node_id();
        if actual != expected_node_id {
            peer.close_with_reason(b"unexpected peer identity");
            return Err(NetworkError::UnexpectedPeerIdentity {
                expected: expected_node_id,
                actual,
            });
        }
        Ok(peer)
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

    pub(crate) fn remote_address(&self) -> SocketAddr {
        self.connection.remote_address()
    }

    pub(crate) fn channel_binding(&self) -> Result<[u8; 32], NetworkError> {
        peer_channel_binding(&self.connection)
    }

    pub fn close(&self) {
        self.close_with_reason(b"done");
    }

    pub(crate) fn close_with_reason(&self, reason: &'static [u8]) {
        self.connection.close(0_u32.into(), reason);
    }

    pub async fn exchange(&self, message: &NetworkMessage) -> Result<NetworkMessage, NetworkError> {
        let deadline = tokio::time::Instant::now() + REQUEST_TIMEOUT;
        let (mut send, mut recv) = request_deadline_at(deadline, "opening request stream", async {
            self.connection.open_bi().await.map_err(transport_error)
        })
        .await?;
        request_deadline_at(
            deadline,
            "sending request",
            write_request_message(&mut send, message),
        )
        .await?;
        read_stream_message_at(&mut recv, deadline, "awaiting response").await
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

    pub(crate) async fn send_one_way(&self, message: &NetworkMessage) -> Result<(), NetworkError> {
        request_deadline("sending one-way message", async {
            let mut send = self.connection.open_uni().await.map_err(transport_error)?;
            write_stream_message(&mut send, message).await
        })
        .await
    }

    pub(crate) async fn accept_one_way(&self) -> Result<Option<NetworkMessage>, NetworkError> {
        match self.connection.accept_uni().await {
            Ok(mut recv) => read_stream_message(&mut recv).await.map(Some),
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

    pub(crate) async fn respond_without_delivery_wait(
        mut self,
        response: &NetworkMessage,
    ) -> Result<(), NetworkError> {
        write_request_message(&mut self.send, response).await
    }
}

pub(crate) fn outbound_bind_address(remote: SocketAddr) -> SocketAddr {
    let ip = match remote {
        SocketAddr::V4(_) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        SocketAddr::V6(_) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
    };
    SocketAddr::new(ip, 0)
}

fn transport_config() -> Arc<TransportConfig> {
    let mut config = TransportConfig::default();
    config.max_concurrent_bidi_streams((MAX_CONCURRENT_REQUEST_STREAMS as u32).into());
    config.max_concurrent_uni_streams((MAX_CONCURRENT_ONE_WAY_STREAMS as u32).into());
    config.max_idle_timeout(Some(
        QUIC_IDLE_TIMEOUT
            .try_into()
            .expect("five seconds is a valid QUIC idle timeout"),
    ));
    config.keep_alive_interval(Some(QUIC_OUTBOUND_KEEP_ALIVE_INTERVAL));
    Arc::new(config)
}

fn pinned_client_config(certificate: &[u8]) -> Result<quinn::ClientConfig, NetworkError> {
    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from(certificate.to_vec()))
        .map_err(transport_error)?;
    let mut config =
        quinn::ClientConfig::with_root_certificates(Arc::new(roots)).map_err(transport_error)?;
    config.transport_config(transport_config());
    Ok(config)
}

#[cfg(test)]
mod tests;

async fn client_handshake(
    connection: Connection,
    identity: &QuicTransportIdentity,
) -> Result<QuicPeer, NetworkError> {
    let channel_binding = peer_channel_binding(&connection)?;
    let (mut send, mut recv) = connection.open_bi().await.map_err(transport_error)?;
    write_request_message(
        &mut send,
        &NetworkMessage::Hello {
            node_id: identity.node_id(),
            signature: identity.sign_peer_auth(&channel_binding, PeerAuthRole::Client),
        },
    )
    .await?;

    let remote_node_id = match read_stream_message(&mut recv).await? {
        NetworkMessage::Hello { node_id, signature } => {
            QuicTransportIdentity::verify_peer_auth(
                node_id,
                signature,
                &channel_binding,
                PeerAuthRole::Server,
            )?;
            node_id
        }
        _ => return Err(NetworkError::UnexpectedMessage),
    };

    Ok(QuicPeer {
        connection,
        remote_node_id,
    })
}

async fn server_handshake(
    connection: Connection,
    identity: &QuicTransportIdentity,
) -> Result<QuicPeer, NetworkError> {
    let channel_binding = peer_channel_binding(&connection)?;
    let (mut send, mut recv) = connection.accept_bi().await.map_err(transport_error)?;

    let remote_node_id = match read_stream_message(&mut recv).await? {
        NetworkMessage::Hello { node_id, signature } => {
            QuicTransportIdentity::verify_peer_auth(
                node_id,
                signature,
                &channel_binding,
                PeerAuthRole::Client,
            )?;
            node_id
        }
        _ => return Err(NetworkError::UnexpectedMessage),
    };

    write_stream_message(
        &mut send,
        &NetworkMessage::Hello {
            node_id: identity.node_id(),
            signature: identity.sign_peer_auth(&channel_binding, PeerAuthRole::Server),
        },
    )
    .await?;

    Ok(QuicPeer {
        connection,
        remote_node_id,
    })
}

fn peer_channel_binding(connection: &Connection) -> Result<[u8; 32], NetworkError> {
    let mut binding = [0_u8; 32];
    connection
        .export_keying_material(&mut binding, PEER_AUTH_EXPORTER_LABEL, b"")
        .map_err(|error| NetworkError::Transport(format!("{error:?}")))?;
    Ok(binding)
}

async fn write_request_message(
    send: &mut SendStream,
    message: &NetworkMessage,
) -> Result<(), NetworkError> {
    let frame = encode_network_message(message)?;
    send.write_all(&frame).await.map_err(transport_error)?;
    send.finish().map_err(transport_error)?;
    Ok(())
}

async fn write_stream_message(
    send: &mut SendStream,
    message: &NetworkMessage,
) -> Result<(), NetworkError> {
    request_deadline("waiting for stream delivery", async {
        write_request_message(send, message).await?;
        match send.stopped().await.map_err(transport_error)? {
            None => Ok(()),
            Some(code) => Err(NetworkError::Transport(format!(
                "QUIC stream stopped by peer with code {code}"
            ))),
        }
    })
    .await
}

async fn read_stream_message(recv: &mut RecvStream) -> Result<NetworkMessage, NetworkError> {
    read_stream_message_at(
        recv,
        tokio::time::Instant::now() + REQUEST_TIMEOUT,
        "reading framed message",
    )
    .await
}

async fn read_stream_message_at(
    recv: &mut RecvStream,
    deadline: tokio::time::Instant,
    stage: &'static str,
) -> Result<NetworkMessage, NetworkError> {
    request_deadline_at(deadline, stage, async {
        let frame = recv
            .read_to_end(MAX_NETWORK_MESSAGE_SIZE)
            .await
            .map_err(transport_error)?;
        decode_network_message(&frame)
    })
    .await
}

fn transport_error(error: impl std::fmt::Display) -> NetworkError {
    NetworkError::Transport(error.to_string())
}
