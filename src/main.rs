use std::env;
use std::io::{self, Write};
use std::net::{Ipv4Addr, SocketAddr};
use std::process::ExitCode;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use second::{
    CurrencyAddress, CurrencyRole, NodeId, NodeRuntime, NodeRuntimeError, QuicClient, StateStore,
    client_ping, client_public_currency_page, client_sync_certified_public_currency_view,
    client_sync_public_currency_view,
};

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), String> {
    let args = env::args().skip(1).collect::<Vec<_>>();

    match args.as_slice() {
        [command, address, node_id, nonce, server_certificate] if command == "ping" => {
            ping(
                address,
                parse_u64("node id", node_id)?,
                parse_u64("nonce", nonce)?,
                server_certificate,
            )
            .await
        }
        [command, snapshot_base] if command == "snapshot-status" => snapshot_status(snapshot_base),
        [command, address, node_id, server_certificate] if command == "sync-public" => {
            sync_public(
                address,
                parse_u64("node id", node_id)?,
                server_certificate,
            )
            .await
        }
        [command, address, node_id, trust_snapshot_base, server_certificate]
            if command == "sync-public-certified" =>
        {
            sync_public_certified(
                address,
                parse_u64("node id", node_id)?,
                trust_snapshot_base,
                server_certificate,
            )
            .await
        }
        [command, address, node_id, snapshot_base] if command == "node" => {
            node(
                address,
                parse_u64("node id", node_id)?,
                snapshot_base,
            )
            .await
        }
        [command, address, node_id, start, limit, server_certificate]
            if command == "query-public" =>
        {
            query_public(
                address,
                parse_u64("node id", node_id)?,
                parse_u64("currency start", start)?,
                parse_u16("limit", limit)?,
                server_certificate,
            )
            .await
        }
        _ => Err(
            "usage: second node <listen-address> <node-id-u64> <snapshot-base> | second ping <address> <node-id-u64> <nonce> <server-cert-base64> | second snapshot-status <snapshot-base> | second query-public <address> <node-id-u64> <start-u64> <limit-u16> <server-cert-base64> | second sync-public <address> <node-id-u64> <server-cert-base64> | second sync-public-certified <address> <node-id-u64> <trust-snapshot-base> <server-cert-base64>"
                .to_owned(),
        ),
    }
}

async fn node(address: &str, node_id: u64, snapshot_base: &str) -> Result<(), String> {
    let store = StateStore::new(snapshot_base);
    let runtime = NodeRuntime::load_and_bind(
        parse_socket_address(address)?,
        NodeId::from_u64(node_id),
        &store,
    )
    .map_err(|error| match error {
        NodeRuntimeError::SnapshotMissing => format!("no snapshot found at {snapshot_base}"),
        other => format!("failed to start node: {other:?}"),
    })?;

    let local_address = runtime
        .local_addr()
        .map_err(|error| format!("failed to read QUIC listening address: {error:?}"))?;
    println!(
        "LISTENING {local_address} CERT {}",
        STANDARD.encode(runtime.transport_certificate_der())
    );
    io::stdout()
        .flush()
        .map_err(|error| format!("failed to flush listening address: {error}"))?;

    runtime
        .run()
        .await
        .map_err(|error| format!("node runtime stopped: {error:?}"))
}

async fn ping(
    address: &str,
    node_id: u64,
    nonce: u64,
    server_certificate: &str,
) -> Result<(), String> {
    let client = quic_client(server_certificate)?;
    let peer = client
        .connect(parse_socket_address(address)?, NodeId::from_u64(node_id))
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

async fn sync_public(address: &str, node_id: u64, server_certificate: &str) -> Result<(), String> {
    let client = quic_client(server_certificate)?;
    let peer = client
        .connect(parse_socket_address(address)?, NodeId::from_u64(node_id))
        .await
        .map_err(|error| format!("failed to connect QUIC peer {address}: {error:?}"))?;

    let synced = client_sync_public_currency_view(&peer)
        .await
        .map_err(|error| format!("public currency sync failed: {error:?}"))?;
    peer.close();
    client.wait_idle().await;

    let summary = &synced.view.summary;
    let digest = hex_digest(&summary.state_digest);

    println!(
        "SYNCED peer={} count={} supply={} reserve={} occupied={} next_currency={} digest={}",
        synced.remote_node_id,
        synced.view.states.len(),
        summary.current_supply,
        summary.reserve_count,
        summary.occupied_count,
        summary.next_currency_address,
        digest,
    );

    Ok(())
}

async fn sync_public_certified(
    address: &str,
    node_id: u64,
    trust_snapshot_base: &str,
    server_certificate: &str,
) -> Result<(), String> {
    let trust_store = StateStore::new(trust_snapshot_base);
    let trusted = trust_store
        .load()
        .map_err(|error| format!("failed to load trust snapshot: {error:?}"))?
        .ok_or_else(|| format!("no trust snapshot found at {trust_snapshot_base}"))?;

    let client = quic_client(server_certificate)?;
    let peer = client
        .connect(parse_socket_address(address)?, NodeId::from_u64(node_id))
        .await
        .map_err(|error| format!("failed to connect QUIC peer {address}: {error:?}"))?;

    let synced = client_sync_certified_public_currency_view(
        &peer,
        &trusted.validator_set,
        trusted.checkpoint_floor_epoch,
    )
    .await
    .map_err(|error| format!("certified public currency sync failed: {error:?}"))?;
    peer.close();
    client.wait_idle().await;

    trust_store
        .advance_checkpoint_floor(&synced.checkpoint)
        .map_err(|error| format!("failed to persist checkpoint floor: {error:?}"))?;

    let summary = &synced.view.summary;
    let checkpoint = synced.checkpoint.checkpoint();
    let digest = hex_digest(&summary.state_digest);

    println!(
        "CERTIFIED peer={} epoch={} validator_set={} votes={} count={} supply={} reserve={} occupied={} next_currency={} digest={}",
        synced.remote_node_id,
        checkpoint.epoch(),
        synced
            .checkpoint
            .certificate()
            .statement()
            .validator_set_version(),
        synced.checkpoint.certificate().vote_count(),
        synced.view.states.len(),
        summary.current_supply,
        summary.reserve_count,
        summary.occupied_count,
        summary.next_currency_address,
        digest,
    );

    Ok(())
}

async fn query_public(
    address: &str,
    node_id: u64,
    start: u64,
    limit: u16,
    server_certificate: &str,
) -> Result<(), String> {
    let client = quic_client(server_certificate)?;
    let peer = client
        .connect(parse_socket_address(address)?, NodeId::from_u64(node_id))
        .await
        .map_err(|error| format!("failed to connect QUIC peer {address}: {error:?}"))?;

    let page = client_public_currency_page(&peer, CurrencyAddress::new(start), limit)
        .await
        .map_err(|error| format!("public currency query failed: {error:?}"))?;
    peer.close();
    client.wait_idle().await;

    let next = page
        .next_start
        .map(|address| address.value().to_string())
        .unwrap_or_else(|| "none".to_owned());

    println!(
        "PUBLIC peer={} next={} count={}",
        page.remote_node_id,
        next,
        page.states.len()
    );

    for state in page.states {
        let role = match state.role {
            CurrencyRole::Circulation => "circulation",
            CurrencyRole::Reserve => "reserve",
        };

        println!(
            "CURRENCY address={} occupied={} role={}",
            state.address.value(),
            state.occupied,
            role
        );
    }

    Ok(())
}

fn quic_client(server_certificate: &str) -> Result<QuicClient, String> {
    let certificate = STANDARD
        .decode(server_certificate)
        .map_err(|error| format!("invalid server certificate base64: {error}"))?;
    QuicClient::new(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)), &certificate)
        .map_err(|error| format!("failed to configure QUIC client: {error:?}"))
}

fn snapshot_status(snapshot_base: &str) -> Result<(), String> {
    let store = StateStore::new(snapshot_base);
    let persisted = store
        .load()
        .map_err(|error| format!("failed to load snapshot: {error:?}"))?
        .ok_or_else(|| format!("no snapshot found at {snapshot_base}"))?;

    println!(
        "SNAPSHOT generation={} supply={} reserve={} next_currency={} validator_set={} validators={} quorum={}",
        persisted.generation,
        persisted.state.current_supply(),
        persisted.state.reserve_count(),
        persisted.state.next_currency_address(),
        persisted.validator_set.version(),
        persisted.validator_set.len(),
        persisted.validator_set.quorum_threshold(),
    );

    Ok(())
}

fn parse_socket_address(value: &str) -> Result<SocketAddr, String> {
    value
        .parse::<SocketAddr>()
        .map_err(|error| format!("invalid socket address {value:?}: {error}"))
}

fn hex_digest(digest: &[u8]) -> String {
    digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

fn parse_u64(label: &str, value: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .map_err(|error| format!("invalid {label} {value:?}: {error}"))
}

fn parse_u16(label: &str, value: &str) -> Result<u16, String> {
    value
        .parse::<u16>()
        .map_err(|error| format!("invalid {label} {value:?}: {error}"))
}
