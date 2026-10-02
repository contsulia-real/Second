mod bootstrap_config;
mod local_file;
mod network_init;
mod node_capabilities;
mod validator_config;
mod validator_keyring;
mod validator_operator;
mod validator_rotation_keys;

use std::env;
use std::io::{self, Write};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::process::ExitCode;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use second::{
    CurrencyAddress, CurrencyRole, DEFAULT_ACTIVE_PEER_TARGET, LegalTask, LegalTaskStatus,
    LegalTaskStatusRejection, LegalTaskSubmissionOutcome, LegalTaskSubmissionRejection,
    NetworkError, NodeRuntime, NodeRuntimeError, PublicCurrencyView, PublicStateStore, QuicClient,
    QuicTransportIdentity, RemoteCertifiedPublicCurrencyView, StateStore, client_legal_task_status,
    client_ping, client_public_currency_page, client_submit_legal_task,
    client_sync_certified_public_currency_view, client_sync_public_currency_view,
    parse_transaction_request_json,
};

const MAX_TRANSACTION_REQUEST_JSON_SIZE: usize = 16 * 1024 * 1024;

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
        [command, validator_id, keyring_file] if command == "validator-keygen" => {
            validator_keygen(parse_u64("validator id", validator_id)?, keyring_file)
        }
        [command, keyring_file, request_file] if command == "validator-admission" => {
            validator_operator::create_admission(keyring_file, request_file)
        }
        [command, snapshot_base, authority, request_file] if command == "validator-rotate" => {
            validator_operator::prepare_rotation(snapshot_base, authority, request_file)
        }
        [command, snapshot_base, plan_file, source_file]
            if command == "validator-transition-build" =>
        {
            validator_operator::build_transition(snapshot_base, plan_file, source_file)
        }
        [command, address, snapshot_base, source_file, server_certificate]
            if command == "validator-transition-submit" =>
        {
            validator_operator::submit_transition(
                address,
                snapshot_base,
                source_file,
                server_certificate,
            )
            .await
        }
        [command, address, snapshot_base, server_certificate]
            if command == "recovery-checkpoint" =>
        {
            validator_operator::request_recovery_checkpoint(
                address,
                snapshot_base,
                server_certificate,
            )
            .await
        }
        [command, address, destination_snapshot_base, trust_snapshot_base, server_certificate]
            if command == "recovery-install" =>
        {
            validator_operator::install_recovery(
                address,
                destination_snapshot_base,
                trust_snapshot_base,
                server_certificate,
            )
            .await
        }
        [command, config_file, output_dir] if command == "init-network" => {
            init_network(config_file, output_dir)
        }
        [command, destination_snapshot_base, trust_snapshot_base]
            if command == "public-init" =>
        {
            public_init(destination_snapshot_base, trust_snapshot_base)
        }
        [command, address, nonce, server_certificate] if command == "ping" => {
            ping(address, parse_u64("nonce", nonce)?, server_certificate).await
        }
        [command, address, transaction_file, authorizer_public_key, server_certificate]
            if command == "submit" =>
        {
            submit(
                address,
                transaction_file,
                authorizer_public_key,
                server_certificate,
            )
            .await
        }
        [command, address, transaction_file, authorizer_public_key, server_certificate]
            if command == "task-status" =>
        {
            task_status(
                address,
                transaction_file,
                authorizer_public_key,
                server_certificate,
            )
            .await
        }
        [command, snapshot_base] if command == "snapshot-status" => snapshot_status(snapshot_base),
        [command, address, server_certificate] if command == "sync-public" => {
            sync_public(address, server_certificate).await
        }
        [command, address, trust_snapshot_base, server_certificate]
            if command == "sync-public-certified" =>
        {
            sync_public_certified(address, trust_snapshot_base, server_certificate).await
        }
        [command, snapshot_base] if command == "observe-public-network" => {
            observe_public_network(snapshot_base).await
        }
        [command, address, snapshot_base] if command == "node" => {
            node(address, snapshot_base).await
        }
        [command, address, start, limit, server_certificate] if command == "query-public" => {
            query_public(
                address,
                parse_u64("currency start", start)?,
                parse_u16("limit", limit)?,
                server_certificate,
            )
            .await
        }
        _ => Err(
            "usage: second validator-keygen <validator-id> <keyring-file> | second validator-admission <keyring-file> <request-file> | second validator-rotate <snapshot-base> <identity|recovery> <request-file> | second validator-transition-build <snapshot-base> <plan-json> <source-file> | second validator-transition-submit <address> <snapshot-base> <source-file> <server-cert-base64> | second recovery-checkpoint <address> <snapshot-base> <server-cert-base64> | second recovery-install <address> <destination-snapshot-base> <trust-snapshot-base> <server-cert-base64> | second init-network <config-json> <output-dir> | second public-init <destination-snapshot-base> <trust-snapshot-base> | second node <listen-address> <snapshot-base> | second submit <address> <transaction-json-file> <authorizer-public-key-base64> <server-cert-base64> | second task-status <address> <transaction-json-file> <authorizer-public-key-base64> <server-cert-base64> | second ping <address> <nonce> <server-cert-base64> | second snapshot-status <snapshot-base> | second query-public <address> <start-u64> <limit-u16> <server-cert-base64> | second sync-public <address> <server-cert-base64> | second sync-public-certified <address> <trust-snapshot-base> <server-cert-base64> | second observe-public-network <snapshot-base>"
                .to_owned(),
        ),
    }
}

fn validator_keygen(validator_id: u64, keyring_file: &str) -> Result<(), String> {
    let credential = validator_keyring::generate(Path::new(keyring_file), validator_id)?;
    println!(
        "VALIDATOR-CREDENTIAL validator={} identity={} consensus={} recovery={} keyring={}",
        credential.id().value(),
        STANDARD.encode(credential.identity_public_key()),
        STANDARD.encode(credential.consensus_public_key()),
        STANDARD.encode(credential.recovery_public_key()),
        keyring_file,
    );
    Ok(())
}

fn init_network(config_file: &str, output_dir: &str) -> Result<(), String> {
    let nodes = network_init::init_network(Path::new(config_file), Path::new(output_dir))?;
    for node in &nodes {
        println!(
            "INITIALIZED validator={} address={} snapshot={} node={} cert={}",
            node.validator_id.value(),
            node.listen_address,
            node.snapshot_base.display(),
            node.node_id,
            network_init::certificate_base64(node),
        );
    }
    Ok(())
}

fn public_init(destination_snapshot_base: &str, trust_snapshot_base: &str) -> Result<(), String> {
    if StateStore::new(destination_snapshot_base)
        .load()
        .map_err(|error| format!("failed to inspect destination full snapshot: {error:?}"))?
        .is_some()
    {
        return Err(format!(
            "destination {destination_snapshot_base} already contains a full node snapshot"
        ));
    }

    let trust = StateStore::new(trust_snapshot_base)
        .load()
        .map_err(|error| format!("failed to load trust snapshot: {error:?}"))?
        .ok_or_else(|| format!("no trust snapshot found at {trust_snapshot_base}"))?;

    let store = PublicStateStore::new(destination_snapshot_base);
    store
        .initialize(
            trust.validator_set.clone(),
            trust.validator_registry.clone(),
        )
        .map_err(|error| format!("failed to initialize public state: {error:?}"))?;

    let mut checkpoint_epoch = None;
    if let Some(proof) = trust.public_checkpoint_proof {
        let certified = proof
            .verify_checkpoint(&trust.validator_set)
            .map_err(|error| format!("invalid trusted public checkpoint: {error:?}"))?;
        let view = PublicCurrencyView::new(
            trust.state.public_currency_summary(),
            trust.state.public_currency_states(),
        )
        .map_err(|error| format!("invalid trusted public state: {error:?}"))?;
        checkpoint_epoch = Some(certified.checkpoint().epoch());
        store
            .install_certified_view(view, &certified)
            .map_err(|error| format!("failed to install trusted public state: {error:?}"))?;
    }

    match checkpoint_epoch {
        Some(epoch) => println!(
            "PUBLIC-INITIALIZED snapshot={} validator_set={} checkpoint_epoch={epoch}",
            destination_snapshot_base,
            trust.validator_set.version()
        ),
        None => println!(
            "PUBLIC-INITIALIZED snapshot={} validator_set={} checkpoint_epoch=none",
            destination_snapshot_base,
            trust.validator_set.version()
        ),
    }
    Ok(())
}

async fn node(address: &str, snapshot_base: &str) -> Result<(), String> {
    let bootstrap_records = bootstrap_config::load(snapshot_base)?;
    let listen_address = parse_socket_address(address)?;
    let full_store = StateStore::new(snapshot_base);
    let full = full_store
        .load()
        .map_err(|error| format!("failed to load node snapshot: {error:?}"))?;

    let (runtime, validator_id, public_only) = match full {
        Some(persisted) => {
            let loaded_capabilities = node_capabilities::load(snapshot_base, &persisted)?;
            let validator_id = loaded_capabilities.validator_id;
            let runtime = NodeRuntime::bind_loaded(
                listen_address,
                &full_store,
                persisted,
                loaded_capabilities.runtime,
            )
            .map_err(|error| format!("failed to start node: {error:?}"))?;
            (runtime, validator_id, false)
        }
        None => {
            node_capabilities::ensure_validator_absent(snapshot_base)?;
            let public_store = PublicStateStore::new(snapshot_base);
            let persisted = public_store
                .load()
                .map_err(|error| format!("failed to load public node snapshot: {error:?}"))?
                .ok_or_else(|| format!("no full or public snapshot found at {snapshot_base}"))?;
            let runtime = NodeRuntime::bind_public_loaded(listen_address, &public_store, persisted)
                .map_err(|error| format!("failed to start public node: {error:?}"))?;
            (runtime, None, true)
        }
    };

    let local_address = runtime
        .local_addr()
        .map_err(|error| format!("failed to read QUIC listening address: {error:?}"))?;
    match validator_id {
        Some(validator_id) => println!(
            "LISTENING {local_address} NODE {} CERT {} VALIDATOR {}",
            runtime.node_id(),
            STANDARD.encode(runtime.transport_certificate_der()),
            validator_id.value()
        ),
        None if public_only => println!(
            "LISTENING {local_address} NODE {} CERT {} PUBLIC",
            runtime.node_id(),
            STANDARD.encode(runtime.transport_certificate_der())
        ),
        None => println!(
            "LISTENING {local_address} NODE {} CERT {}",
            runtime.node_id(),
            STANDARD.encode(runtime.transport_certificate_der())
        ),
    }
    io::stdout()
        .flush()
        .map_err(|error| format!("failed to flush listening address: {error}"))?;

    runtime
        .run(&bootstrap_records)
        .await
        .map_err(|error| format!("node runtime stopped: {error:?}"))
}

async fn submit(
    address: &str,
    transaction_file: &str,
    authorizer_public_key: &str,
    server_certificate: &str,
) -> Result<(), String> {
    let task = load_transaction_task(transaction_file, authorizer_public_key)?;

    let client = quic_client(server_certificate)?;
    let peer = client
        .connect(parse_socket_address(address)?)
        .await
        .map_err(|error| format!("failed to connect QUIC peer {address}: {error:?}"))?;
    let result = client_submit_legal_task(&peer, &task).await;
    peer.close();
    client.wait_idle().await;

    let submitted = result.map_err(submission_error)?;
    let state = match submitted.outcome {
        LegalTaskSubmissionOutcome::Prepared => "prepared",
        LegalTaskSubmissionOutcome::AlreadyPending => "pending",
        LegalTaskSubmissionOutcome::AlreadySucceeded => "succeeded",
    };
    println!("ACCEPTED task={} state={state}", submitted.task_id);
    Ok(())
}

async fn task_status(
    address: &str,
    transaction_file: &str,
    authorizer_public_key: &str,
    server_certificate: &str,
) -> Result<(), String> {
    let task = load_transaction_task(transaction_file, authorizer_public_key)?;
    let client = quic_client(server_certificate)?;
    let peer = client
        .connect(parse_socket_address(address)?)
        .await
        .map_err(|error| format!("failed to connect QUIC peer {address}: {error:?}"))?;
    let result = client_legal_task_status(&peer, &task).await;
    peer.close();
    client.wait_idle().await;

    let remote = result.map_err(task_status_error)?;
    let state = match remote.status {
        LegalTaskStatus::Unknown => "unknown",
        LegalTaskStatus::Bound => "bound",
        LegalTaskStatus::Prepared => "prepared",
        LegalTaskStatus::Voting => "voting",
        LegalTaskStatus::Finalized => "finalized",
        LegalTaskStatus::Succeeded => "succeeded",
    };
    println!("TASK task={} state={state}", remote.task_id);
    Ok(())
}

fn task_status_error(error: NetworkError) -> String {
    match error {
        NetworkError::LegalTaskStatusRejected(LegalTaskStatusRejection::Unavailable) => {
            "target node does not provide private LegalTask status".to_owned()
        }
        NetworkError::LegalTaskStatusRejected(LegalTaskStatusRejection::Rejected) => {
            "LegalTask status query was rejected".to_owned()
        }
        other => format!("LegalTask status query failed: {other:?}"),
    }
}

fn load_transaction_task(
    transaction_file: &str,
    authorizer_public_key: &str,
) -> Result<LegalTask, String> {
    let request = local_file::read_bounded(
        Path::new(transaction_file),
        MAX_TRANSACTION_REQUEST_JSON_SIZE,
        "transaction request",
    )?;
    let authorizer_public_key = local_file::decode_standard_base64_32(authorizer_public_key)
        .map_err(|error| format!("invalid authorizer public key: {error}"))?;
    parse_transaction_request_json(&request, authorizer_public_key)
        .map_err(|error| format!("invalid transaction request: {error:?}"))
}

fn submission_error(error: NetworkError) -> String {
    match error {
        NetworkError::LegalTaskSubmissionRejected(LegalTaskSubmissionRejection::Unavailable) => {
            "target node does not provide LegalTask submission".to_owned()
        }
        NetworkError::LegalTaskSubmissionRejected(LegalTaskSubmissionRejection::Busy) => {
            "target node is temporarily at its LegalTask submission capacity".to_owned()
        }
        NetworkError::LegalTaskSubmissionRejected(LegalTaskSubmissionRejection::Rejected) => {
            "LegalTask submission was rejected".to_owned()
        }
        other => format!("LegalTask submission failed: {other:?}"),
    }
}

async fn ping(address: &str, nonce: u64, server_certificate: &str) -> Result<(), String> {
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

async fn sync_public(address: &str, server_certificate: &str) -> Result<(), String> {
    let client = quic_client(server_certificate)?;
    let peer = client
        .connect(parse_socket_address(address)?)
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
        .connect(parse_socket_address(address)?)
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

    print_certified_public_view("CERTIFIED", &synced);

    Ok(())
}

async fn observe_public_network(snapshot_base: &str) -> Result<(), String> {
    let bootstrap_records = bootstrap_config::load(snapshot_base)?;
    let store = StateStore::new(snapshot_base);
    let runtime = NodeRuntime::load_and_bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), &store)
        .map_err(|error| match error {
            NodeRuntimeError::SnapshotMissing => {
                format!("no snapshot found at {snapshot_base}")
            }
            other => format!("failed to start network observer: {other:?}"),
        })?;

    runtime
        .bootstrap(&bootstrap_records, DEFAULT_ACTIVE_PEER_TARGET)
        .await
        .map_err(|error| format!("failed to connect known peers: {error:?}"))?;

    let synced = runtime
        .sync_freshest_certified_public_currency_view()
        .await
        .map_err(|error| format!("certified public network observation failed: {error:?}"))?;

    print_certified_public_view("NETWORK-CERTIFIED", &synced);
    Ok(())
}

fn print_certified_public_view(label: &str, synced: &RemoteCertifiedPublicCurrencyView) {
    let summary = &synced.view.summary;
    let checkpoint = synced.checkpoint.checkpoint();
    let digest = hex_digest(&summary.state_digest);

    println!(
        "{label} peer={} epoch={} validator_set={} votes={} count={} supply={} reserve={} occupied={} next_currency={} digest={}",
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
}

async fn query_public(
    address: &str,
    start: u64,
    limit: u16,
    server_certificate: &str,
) -> Result<(), String> {
    let client = quic_client(server_certificate)?;
    let peer = client
        .connect(parse_socket_address(address)?)
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

fn snapshot_status(snapshot_base: &str) -> Result<(), String> {
    let store = StateStore::new(snapshot_base);
    let persisted = store
        .load()
        .map_err(|error| format!("failed to load snapshot: {error:?}"))?
        .ok_or_else(|| format!("no snapshot found at {snapshot_base}"))?;

    let recovery_serial = persisted
        .recovery_checkpoint_proof
        .as_ref()
        .map(|proof| proof.checkpoint().serial().to_string())
        .unwrap_or_else(|| "none".to_owned());
    println!(
        "SNAPSHOT generation={} supply={} reserve={} next_currency={} validator_set={} validators={} quorum={} safety={} recovery_serial={}",
        persisted.generation,
        persisted.state.current_supply(),
        persisted.state.reserve_count(),
        persisted.state.next_currency_address(),
        persisted.validator_set.version(),
        persisted.validator_set.len(),
        persisted.validator_set.quorum_threshold(),
        if persisted.validator_safety_ready {
            "ready"
        } else {
            "locked"
        },
        recovery_serial,
    );

    Ok(())
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
