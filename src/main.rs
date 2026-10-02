mod bootstrap_config;
mod cli_legal_task;
mod cli_network;
mod cli_node;
mod cli_public;
mod local_file;
mod network_init;
mod node_capabilities;
mod validator_config;
mod validator_keyring;
mod validator_operator;
mod validator_rotation_keys;

use cli_legal_task::{submit, task_status};
use cli_network::ping;
pub(crate) use cli_network::{hex_digest, parse_socket_address, quic_client};
use cli_node::node;
use cli_public::{
    observe_public_network, public_init, query_public, snapshot_status, sync_public,
    sync_public_certified,
};

use std::env;
use std::path::Path;
use std::process::ExitCode;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
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
