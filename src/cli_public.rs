use std::net::{Ipv4Addr, SocketAddr};

use second::{
    CurrencyAddress, DEFAULT_ACTIVE_PEER_TARGET, NodeRuntime, NodeRuntimeError, PublicCurrencyView,
    PublicStateStore, RemoteCertifiedPublicCurrencyView, StateStore, client_public_currency_page,
    client_sync_certified_public_currency_view, client_sync_public_currency_view,
};

use crate::{bootstrap_config, hex_digest, parse_socket_address, quic_client};

pub(crate) fn public_init(
    destination_snapshot_base: &str,
    trust_snapshot_base: &str,
) -> Result<(), String> {
    let _directory_lock =
        crate::local_file::lock_node_directory(std::path::Path::new(destination_snapshot_base))?;
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

pub(crate) async fn sync_public(address: &str, server_certificate: &str) -> Result<(), String> {
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

pub(crate) async fn sync_public_certified(
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

pub(crate) async fn observe_public_network(snapshot_base: &str) -> Result<(), String> {
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

pub(crate) async fn query_public(
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
        println!(
            "CURRENCY start={} len={} occupied={}",
            state.start.value(),
            state.len,
            state.occupied
        );
    }

    Ok(())
}

pub(crate) fn snapshot_status(snapshot_base: &str) -> Result<(), String> {
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
