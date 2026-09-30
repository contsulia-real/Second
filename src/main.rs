use std::env;
use std::io::{self, Write};
use std::net::{TcpListener, TcpStream};
use std::process::ExitCode;
use std::time::Duration;

use second::{
    CurrencyAddress, CurrencyRole, NodeId, StateStore, client_ping, client_public_currency_page,
    client_sync_certified_public_currency_view, client_sync_public_currency_view,
    serve_ping_session, serve_public_currency_connection_with_checkpoint,
};

const IO_TIMEOUT: Duration = Duration::from_secs(5);

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args = env::args().skip(1).collect::<Vec<_>>();

    match args.as_slice() {
        [command, address, node_id] if command == "serve-once" => {
            serve_once(address, parse_u64("node id", node_id)?)
        }
        [command, address, node_id, nonce] if command == "ping" => ping(
            address,
            parse_u64("node id", node_id)?,
            parse_u64("nonce", nonce)?,
        ),
        [command, snapshot_base] if command == "snapshot-status" => snapshot_status(snapshot_base),
        [command, address, node_id] if command == "sync-public" => {
            sync_public(address, parse_u64("node id", node_id)?)
        }
        [command, address, node_id, trust_snapshot_base] if command == "sync-public-certified" => {
            sync_public_certified(
                address,
                parse_u64("node id", node_id)?,
                trust_snapshot_base,
            )
        }
        [command, address, node_id, snapshot_base] if command == "serve-public-once" => {
            serve_public_once(
                address,
                parse_u64("node id", node_id)?,
                snapshot_base,
            )
        }
        [command, address, node_id, start, limit] if command == "query-public" => {
            query_public(
                address,
                parse_u64("node id", node_id)?,
                parse_u64("currency start", start)?,
                parse_u16("limit", limit)?,
            )
        }
        _ => Err(
            "usage: second serve-once <listen-address> <node-id-u64> | second ping <address> <node-id-u64> <nonce> | second snapshot-status <snapshot-base> | second serve-public-once <listen-address> <node-id-u64> <snapshot-base> | second query-public <address> <node-id-u64> <start-u64> <limit-u16> | second sync-public <address> <node-id-u64> | second sync-public-certified <address> <node-id-u64> <trust-snapshot-base>"
                .to_owned(),
        ),
    }
}

fn serve_once(address: &str, node_id: u64) -> Result<(), String> {
    let listener =
        TcpListener::bind(address).map_err(|error| format!("failed to bind {address}: {error}"))?;
    let local_address = listener
        .local_addr()
        .map_err(|error| format!("failed to read listening address: {error}"))?;

    println!("LISTENING {local_address}");
    io::stdout()
        .flush()
        .map_err(|error| format!("failed to flush listening address: {error}"))?;

    let (mut stream, _) = listener
        .accept()
        .map_err(|error| format!("failed to accept peer: {error}"))?;
    configure_stream(&stream)?;

    let peer = serve_ping_session(&mut stream, NodeId::from_u64(node_id))
        .map_err(|error| format!("network session failed: {error:?}"))?;

    println!("PEER {peer}");
    Ok(())
}

fn ping(address: &str, node_id: u64, nonce: u64) -> Result<(), String> {
    let mut stream = TcpStream::connect(address)
        .map_err(|error| format!("failed to connect {address}: {error}"))?;
    configure_stream(&stream)?;

    let peer = client_ping(&mut stream, NodeId::from_u64(node_id), nonce)
        .map_err(|error| format!("network ping failed: {error:?}"))?;

    println!("PONG peer={peer} nonce={nonce}");
    Ok(())
}

fn serve_public_once(address: &str, node_id: u64, snapshot_base: &str) -> Result<(), String> {
    let store = StateStore::new(snapshot_base);
    let persisted = store
        .load()
        .map_err(|error| format!("failed to load snapshot: {error:?}"))?
        .ok_or_else(|| format!("no snapshot found at {snapshot_base}"))?;

    let listener =
        TcpListener::bind(address).map_err(|error| format!("failed to bind {address}: {error}"))?;
    let local_address = listener
        .local_addr()
        .map_err(|error| format!("failed to read listening address: {error}"))?;

    println!("LISTENING {local_address}");
    io::stdout()
        .flush()
        .map_err(|error| format!("failed to flush listening address: {error}"))?;

    let (mut stream, _) = listener
        .accept()
        .map_err(|error| format!("failed to accept peer: {error}"))?;
    configure_stream(&stream)?;

    let peer = serve_public_currency_connection_with_checkpoint(
        &mut stream,
        NodeId::from_u64(node_id),
        &persisted.state,
        persisted.public_checkpoint_proof.as_ref(),
    )
    .map_err(|error| format!("public currency connection failed: {error:?}"))?;

    println!("PEER {peer}");
    Ok(())
}

fn sync_public(address: &str, node_id: u64) -> Result<(), String> {
    let mut stream = TcpStream::connect(address)
        .map_err(|error| format!("failed to connect {address}: {error}"))?;
    configure_stream(&stream)?;

    let synced = client_sync_public_currency_view(&mut stream, NodeId::from_u64(node_id))
        .map_err(|error| format!("public currency sync failed: {error:?}"))?;
    let summary = &synced.view.summary;
    let digest = summary
        .state_digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();

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

fn sync_public_certified(
    address: &str,
    node_id: u64,
    trust_snapshot_base: &str,
) -> Result<(), String> {
    let trust_store = StateStore::new(trust_snapshot_base);
    let trusted = trust_store
        .load()
        .map_err(|error| format!("failed to load trust snapshot: {error:?}"))?
        .ok_or_else(|| format!("no trust snapshot found at {trust_snapshot_base}"))?;

    let minimum_checkpoint_epoch = trusted.checkpoint_floor_epoch;

    let mut stream = TcpStream::connect(address)
        .map_err(|error| format!("failed to connect {address}: {error}"))?;
    configure_stream(&stream)?;

    let synced = client_sync_certified_public_currency_view(
        &mut stream,
        NodeId::from_u64(node_id),
        &trusted.validator_set,
        minimum_checkpoint_epoch,
    )
    .map_err(|error| format!("certified public currency sync failed: {error:?}"))?;

    trust_store
        .advance_checkpoint_floor(&synced.checkpoint)
        .map_err(|error| format!("failed to persist checkpoint floor: {error:?}"))?;

    let summary = &synced.view.summary;
    let checkpoint = synced.checkpoint.checkpoint();
    let digest = summary
        .state_digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();

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

fn query_public(address: &str, node_id: u64, start: u64, limit: u16) -> Result<(), String> {
    let mut stream = TcpStream::connect(address)
        .map_err(|error| format!("failed to connect {address}: {error}"))?;
    configure_stream(&stream)?;

    let page = client_public_currency_page(
        &mut stream,
        NodeId::from_u64(node_id),
        CurrencyAddress::new(start),
        limit,
    )
    .map_err(|error| format!("public currency query failed: {error:?}"))?;

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

fn configure_stream(stream: &TcpStream) -> Result<(), String> {
    stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .map_err(|error| format!("failed to set read timeout: {error}"))?;
    stream
        .set_write_timeout(Some(IO_TIMEOUT))
        .map_err(|error| format!("failed to set write timeout: {error}"))?;
    Ok(())
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
