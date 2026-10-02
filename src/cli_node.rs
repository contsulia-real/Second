use std::io::{self, Write};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use second::{NodeRuntime, PublicStateStore, StateStore};

use crate::{bootstrap_config, node_capabilities, parse_socket_address};

pub(crate) async fn node(address: &str, snapshot_base: &str) -> Result<(), String> {
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
