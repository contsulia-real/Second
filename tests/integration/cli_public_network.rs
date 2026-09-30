use std::fs;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::support;
use support::FinalizedExecute as _;

use second::{
    CURRENT_PROTOCOL_VERSION, Operation, PublicCurrencyCheckpoint, PublicCurrencyCheckpointProof,
    SecondState, StateStore, ValidatorCredential, ValidatorId, ValidatorSet,
};
use support::{key, signed_vote, temp_base, verified_task};

fn validators() -> ValidatorSet {
    ValidatorSet::new(
        7,
        (1..=4).map(|id| {
            ValidatorCredential::new(
                ValidatorId::new(id),
                key((id as u8).wrapping_add(40)).verifying_key().to_bytes(),
                key(id as u8).verifying_key().to_bytes(),
                key((id as u8).wrapping_add(80)).verifying_key().to_bytes(),
            )
            .unwrap()
        }),
    )
    .unwrap()
}

fn checkpoint_proof(
    state: &SecondState,
    validators: &ValidatorSet,
    epoch: u64,
) -> PublicCurrencyCheckpointProof {
    let checkpoint = PublicCurrencyCheckpoint::new(
        CURRENT_PROTOCOL_VERSION,
        epoch,
        state.public_currency_summary(),
    );
    let statement = checkpoint.finality_statement(validators.version());
    let votes = [1_u64, 2, 3]
        .into_iter()
        .map(|id| signed_vote(&statement, ValidatorId::new(id), &key(id as u8)))
        .collect();

    PublicCurrencyCheckpointProof::new(checkpoint, validators.version(), votes)
}

fn parse_listening(line: &str) -> (String, String, String) {
    let mut fields = line.split_whitespace();
    assert_eq!(fields.next(), Some("LISTENING"));
    let address = fields
        .next()
        .expect("missing QUIC listening address")
        .to_owned();
    assert_eq!(fields.next(), Some("NODE"));
    let node_id = fields
        .next()
        .expect("missing authenticated NodeId")
        .to_owned();
    assert_eq!(fields.next(), Some("CERT"));
    let certificate = fields
        .next()
        .expect("missing QUIC transport certificate")
        .to_owned();
    assert_eq!(fields.next(), None);
    (address, node_id, certificate)
}

#[test]
fn real_process_certified_sync_uses_independent_local_validator_trust() {
    let server_base = temp_base("certified-server");
    let trust_base = temp_base("certified-trust");
    let server_store = StateStore::new(&server_base);
    let trust_store = StateStore::new(&trust_base);

    let state = SecondState::genesis([], 10).with_reserve(300).unwrap();
    let set = validators();
    let proof = checkpoint_proof(&state, &set, 77);

    server_store.initialize(&state, &set).unwrap();
    server_store.attach_checkpoint_proof(Some(&proof)).unwrap();

    let unrelated_trust_state = SecondState::genesis([], 999);
    trust_store
        .initialize(&unrelated_trust_state, &set)
        .unwrap();

    let executable = env!("CARGO_BIN_EXE_second");
    let mut server = Command::new(executable)
        .args(["node", "127.0.0.1:0", server_base.to_str().unwrap()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let stdout = server.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);
    let mut listening = String::new();
    reader.read_line(&mut listening).unwrap();

    let (address, server_node_id, server_certificate) = parse_listening(&listening);

    let client = Command::new(executable)
        .args([
            "sync-public-certified",
            address.as_str(),
            trust_base.to_str().unwrap(),
            server_certificate.as_str(),
        ])
        .output()
        .unwrap();

    if !client.status.success() {
        let _ = server.kill();
        let _ = server.wait();
        panic!(
            "sync-public-certified failed: {}",
            String::from_utf8_lossy(&client.stderr)
        );
    }

    let client_stdout = String::from_utf8(client.stdout).unwrap();
    assert!(client_stdout.contains("CERTIFIED "));
    assert!(client_stdout.contains(&format!("peer={server_node_id}")));
    assert!(client_stdout.contains("epoch=77"));
    assert!(client_stdout.contains("validator_set=7"));
    assert!(client_stdout.contains("votes=3"));
    assert!(client_stdout.contains("count=300"));
    assert!(client_stdout.contains("supply=300"));
    assert!(client_stdout.contains("reserve=300"));
    assert!(client_stdout.contains("occupied=0"));
    assert!(client_stdout.contains("next_currency=310"));

    let advanced_trust = trust_store.load().unwrap().unwrap();
    assert_eq!(advanced_trust.checkpoint_floor_epoch, 77);
    assert_eq!(advanced_trust.state.next_currency_address(), 999);
    assert!(advanced_trust.public_checkpoint_proof.is_none());

    let lower = client_stdout.to_ascii_lowercase();
    assert!(!lower.contains("owner"));
    assert!(!lower.contains("balance"));

    server.kill().expect("node exited before test shutdown");
    server.wait().unwrap();

    server_store.remove_files().unwrap();
    trust_store.remove_files().unwrap();
    support::remove_transport_identity(&server_base);
}

#[test]
fn real_process_sync_rebuilds_multi_page_public_view_from_snapshot() {
    let base = temp_base("sync");
    let store = StateStore::new(&base);
    let state = SecondState::genesis([], 10).with_reserve(600).unwrap();
    store.initialize(&state, &validators()).unwrap();

    let executable = env!("CARGO_BIN_EXE_second");
    let mut server = Command::new(executable)
        .args(["node", "127.0.0.1:0", base.to_str().unwrap()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let stdout = server.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);
    let mut listening = String::new();
    reader.read_line(&mut listening).unwrap();

    let (address, server_node_id, server_certificate) = parse_listening(&listening);

    let client = Command::new(executable)
        .args(["sync-public", address.as_str(), server_certificate.as_str()])
        .output()
        .unwrap();

    if !client.status.success() {
        let _ = server.kill();
        let _ = server.wait();
        panic!(
            "sync-public failed: {}",
            String::from_utf8_lossy(&client.stderr)
        );
    }

    let client_stdout = String::from_utf8(client.stdout).unwrap();
    assert!(client_stdout.contains("SYNCED "));
    assert!(client_stdout.contains(&format!("peer={server_node_id}")));
    assert!(client_stdout.contains("count=600"));
    assert!(client_stdout.contains("supply=600"));
    assert!(client_stdout.contains("reserve=600"));
    assert!(client_stdout.contains("occupied=0"));
    assert!(client_stdout.contains("next_currency=610"));

    let lower = client_stdout.to_ascii_lowercase();
    assert!(!lower.contains("owner"));
    assert!(!lower.contains("balance"));

    server.kill().expect("node exited before test shutdown");
    server.wait().unwrap();

    store.remove_files().unwrap();
    support::remove_transport_identity(&base);
}

#[test]
fn long_lived_node_serves_multiple_client_connections_from_snapshot() {
    let base = temp_base("roundtrip");
    let store = StateStore::new(&base);

    let alice = support::account(876543);
    let mut state = SecondState::genesis([alice], 1).with_reserve(1).unwrap();
    state
        .execute_finalized(
            &verified_task(
                1,
                vec![Operation::Issue {
                    account: alice,
                    count: 2,
                }],
            ),
            1,
        )
        .unwrap();
    store.initialize(&state, &validators()).unwrap();

    let executable = env!("CARGO_BIN_EXE_second");
    let mut server = Command::new(executable)
        .args(["node", "127.0.0.1:0", base.to_str().unwrap()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let stdout = server.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);
    let mut listening = String::new();
    reader.read_line(&mut listening).unwrap();

    let (address, server_node_id, server_certificate) = parse_listening(&listening);

    let client = Command::new(executable)
        .args([
            "query-public",
            address.as_str(),
            "1",
            "3",
            server_certificate.as_str(),
        ])
        .output()
        .unwrap();

    assert!(client.status.success());
    let client_stdout = String::from_utf8(client.stdout).unwrap();
    assert!(client_stdout.contains("PUBLIC "));
    assert!(client_stdout.contains(&format!("peer={server_node_id}")));
    assert!(client_stdout.contains("count=3"));
    assert!(client_stdout.contains("address=1 occupied=false role=reserve"));
    assert!(client_stdout.contains("address=2 occupied=true role=circulation"));
    assert!(client_stdout.contains("address=3 occupied=true role=circulation"));

    let lower = client_stdout.to_ascii_lowercase();
    assert!(!lower.contains("owner"));
    assert!(!lower.contains("balance"));
    assert!(!client_stdout.contains("876543"));

    let ping = Command::new(executable)
        .args(["ping", address.as_str(), "42", server_certificate.as_str()])
        .output()
        .unwrap();
    assert!(
        ping.status.success(),
        "second connection failed: {}",
        String::from_utf8_lossy(&ping.stderr)
    );
    assert!(String::from_utf8(ping.stdout).unwrap().contains("PONG "));

    server.kill().expect("node exited before test shutdown");
    server.wait().unwrap();

    let mut restarted = Command::new(executable)
        .args(["node", "127.0.0.1:0", base.to_str().unwrap()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = restarted.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);
    let mut restarted_listening = String::new();
    reader.read_line(&mut restarted_listening).unwrap();
    let (_, restarted_node_id, restarted_certificate) = parse_listening(&restarted_listening);

    assert_eq!(restarted_node_id, server_node_id);
    assert_eq!(restarted_certificate, server_certificate);

    restarted
        .kill()
        .expect("restarted node exited before test shutdown");
    restarted.wait().unwrap();

    store.remove_files().unwrap();
    support::remove_transport_identity(&base);
}

#[test]
fn node_uses_static_bootstrap_sidecar() {
    let seed_base = temp_base("bootstrap-seed");
    let client_base = temp_base("bootstrap-client");
    let seed_store = StateStore::new(&seed_base);
    let client_store = StateStore::new(&client_base);
    let state = SecondState::genesis([], 1).with_reserve(2).unwrap();
    let set = validators();
    seed_store.initialize(&state, &set).unwrap();
    client_store.initialize(&state, &set).unwrap();

    let executable = env!("CARGO_BIN_EXE_second");
    let mut seed = Command::new(executable)
        .args(["node", "127.0.0.1:0", seed_base.to_str().unwrap()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let seed_stdout = seed.stdout.take().unwrap();
    let mut seed_reader = BufReader::new(seed_stdout);
    let mut seed_listening = String::new();
    seed_reader.read_line(&mut seed_listening).unwrap();
    let (seed_address, seed_node_id, seed_certificate) = parse_listening(&seed_listening);

    let bootstrap = serde_json::json!([{
        "node_id": seed_node_id,
        "address": seed_address,
        "certificate_base64": seed_certificate,
    }]);
    fs::write(
        support::bootstrap_config_path(&client_base),
        serde_json::to_vec_pretty(&bootstrap).unwrap(),
    )
    .unwrap();

    let mut client = Command::new(executable)
        .args(["node", "127.0.0.1:0", client_base.to_str().unwrap()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let client_stdout = client.stdout.take().unwrap();
    let mut client_reader = BufReader::new(client_stdout);
    let mut client_listening = String::new();
    client_reader.read_line(&mut client_listening).unwrap();
    parse_listening(&client_listening);

    let peer_store = support::peer_store_path(&client_base);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !peer_store.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(
        peer_store.exists(),
        "node did not authenticate and persist the configured bootstrap peer"
    );

    client
        .kill()
        .expect("client node exited before test shutdown");
    client.wait().unwrap();
    seed.kill().expect("seed node exited before test shutdown");
    seed.wait().unwrap();

    support::cleanup_node_runtime(client_store, client_base);
    support::cleanup_node_runtime(seed_store, seed_base);
}

#[test]
fn node_rejects_invalid_bootstrap_sidecar_before_starting_transport() {
    let base = temp_base("bootstrap-invalid");
    let store = StateStore::new(&base);
    let state = SecondState::genesis([], 1).with_reserve(2).unwrap();
    store.initialize(&state, &validators()).unwrap();

    fs::write(
        support::bootstrap_config_path(&base),
        br#"{"unexpected":true}"#,
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_second"))
        .args(["node", "127.0.0.1:0", base.to_str().unwrap()])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("invalid bootstrap file"),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !support::transport_identity_path(&base).exists(),
        "invalid bootstrap config must fail before transport identity creation"
    );

    support::cleanup_node_runtime(store, base);
}
