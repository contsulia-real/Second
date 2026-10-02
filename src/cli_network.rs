use std::net::{Ipv4Addr, SocketAddr};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use second::{QuicClient, QuicTransportIdentity, client_ping};

pub(crate) async fn ping(
    address: &str,
    nonce: u64,
    server_certificate: &str,
) -> Result<(), String> {
    let client = quic_client(server_certificate)?;
    let peer = client
        .connect(parse_socket_address(address)?)
        .await
        .map_err(|error| format!("failed to connect QUIC peer {address}: {error:?}"))?;

    let remote = client_ping(&peer, nonce)
        .await
        .map_err(|error| format!("network ping failed: {error:?}"))?;
    peer.close();
    client.wait_idle().await;

    println!("PONG peer={remote} nonce={nonce}");
    Ok(())
}

pub(crate) fn quic_client(server_certificate: &str) -> Result<QuicClient, String> {
    let certificate = STANDARD
        .decode(server_certificate)
        .map_err(|error| format!("invalid server certificate base64: {error}"))?;
    let identity = QuicTransportIdentity::generate()
        .map_err(|error| format!("failed to generate client transport identity: {error:?}"))?;
    QuicClient::new(
        SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
        &certificate,
        identity,
    )
    .map_err(|error| format!("failed to configure QUIC client: {error:?}"))
}

pub(crate) fn parse_socket_address(value: &str) -> Result<SocketAddr, String> {
    value
        .parse::<SocketAddr>()
        .map_err(|error| format!("invalid socket address {value:?}: {error}"))
}

pub(crate) fn hex_digest(digest: &[u8]) -> String {
    digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}
