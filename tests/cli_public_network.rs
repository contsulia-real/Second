use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

use ed25519_dalek::SigningKey;
use second::{
    AccountAddress, AuthorizerSet, CURRENT_PROTOCOL_VERSION, LegalTask, LegalTaskPayload,
    Operation, PublicCurrencyCheckpoint, PublicCurrencyCheckpointProof, SecondState, StateStore,
    TaskId, ValidatorCredential, ValidatorId, ValidatorSet, ValidatorVote,
};

fn key(byte: u8) -> SigningKey {
    SigningKey::from_bytes(&[byte; 32])
}

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

fn verified_task(task_id: u128, operations: Vec<Operation>) -> second::VerifiedLegalTask {
    let signing = key(9);
    let authorizers = AuthorizerSet::new(
        CURRENT_PROTOCOL_VERSION,
        [signing.verifying_key().to_bytes()],
    )
    .unwrap();

    LegalTask::sign(
        LegalTaskPayload::new(
            TaskId::new(task_id),
            CURRENT_PROTOCOL_VERSION,
            None,
            operations,
        ),
        &signing,
    )
    .unwrap()
    .verify(&authorizers)
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
        .map(|id| ValidatorVote::sign(&statement, ValidatorId::new(id), &key(id as u8)))
        .collect();

    PublicCurrencyCheckpointProof::new(checkpoint, validators.version(), votes)
}

fn temp_base(name: &str) -> std::path::PathBuf {
    let unique = format!(
        "second-cli-public-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    std::env::temp_dir().join(unique)
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

    server_store
        .save_with_checkpoint(&state, &set, Some(&proof))
        .unwrap();

    let unrelated_trust_state = SecondState::genesis([], 999);
    trust_store.save(&unrelated_trust_state, &set).unwrap();

    let executable = env!("CARGO_BIN_EXE_second");
    let mut server = Command::new(executable)
        .args([
            "serve-public-once",
            "127.0.0.1:0",
            "1",
            server_base.to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let stdout = server.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);
    let mut listening = String::new();
    reader.read_line(&mut listening).unwrap();

    assert!(listening.starts_with("LISTENING "));
    let address = listening.trim().strip_prefix("LISTENING ").unwrap();

    let client = Command::new(executable)
        .args([
            "sync-public-certified",
            address,
            "2",
            trust_base.to_str().unwrap(),
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
    assert!(client_stdout.contains("epoch=77"));
    assert!(client_stdout.contains("validator_set=7"));
    assert!(client_stdout.contains("votes=3"));
    assert!(client_stdout.contains("count=300"));
    assert!(client_stdout.contains("supply=300"));
    assert!(client_stdout.contains("reserve=300"));
    assert!(client_stdout.contains("occupied=0"));
    assert!(client_stdout.contains("next_currency=310"));

    let lower = client_stdout.to_ascii_lowercase();
    assert!(!lower.contains("owner"));
    assert!(!lower.contains("balance"));

    let status = server.wait().unwrap();
    assert!(status.success());

    server_store.remove_files().unwrap();
    trust_store.remove_files().unwrap();
}

#[test]
fn real_process_sync_rebuilds_multi_page_public_view_from_snapshot() {
    let base = temp_base("sync");
    let store = StateStore::new(&base);
    let state = SecondState::genesis([], 10).with_reserve(600).unwrap();
    store.save(&state, &validators()).unwrap();

    let executable = env!("CARGO_BIN_EXE_second");
    let mut server = Command::new(executable)
        .args([
            "serve-public-once",
            "127.0.0.1:0",
            "1",
            base.to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let stdout = server.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);
    let mut listening = String::new();
    reader.read_line(&mut listening).unwrap();

    assert!(listening.starts_with("LISTENING "));
    let address = listening.trim().strip_prefix("LISTENING ").unwrap();

    let client = Command::new(executable)
        .args(["sync-public", address, "2"])
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
    assert!(client_stdout.contains("count=600"));
    assert!(client_stdout.contains("supply=600"));
    assert!(client_stdout.contains("reserve=600"));
    assert!(client_stdout.contains("occupied=0"));
    assert!(client_stdout.contains("next_currency=610"));

    let lower = client_stdout.to_ascii_lowercase();
    assert!(!lower.contains("owner"));
    assert!(!lower.contains("balance"));

    let status = server.wait().unwrap();
    assert!(status.success());

    store.remove_files().unwrap();
}

#[test]
fn two_real_processes_serve_and_query_public_currency_state_from_snapshot() {
    let base = temp_base("roundtrip");
    let store = StateStore::new(&base);

    let alice = AccountAddress::new(876543);
    let mut state = SecondState::genesis([alice], 1).with_reserve(1).unwrap();
    state
        .execute(
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
    store.save(&state, &validators()).unwrap();

    let executable = env!("CARGO_BIN_EXE_second");
    let mut server = Command::new(executable)
        .args([
            "serve-public-once",
            "127.0.0.1:0",
            "1",
            base.to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let stdout = server.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);
    let mut listening = String::new();
    reader.read_line(&mut listening).unwrap();

    assert!(listening.starts_with("LISTENING "));
    let address = listening.trim().strip_prefix("LISTENING ").unwrap();

    let client = Command::new(executable)
        .args(["query-public", address, "2", "1", "3"])
        .output()
        .unwrap();

    assert!(client.status.success());
    let client_stdout = String::from_utf8(client.stdout).unwrap();
    assert!(client_stdout.contains("PUBLIC "));
    assert!(client_stdout.contains("count=3"));
    assert!(client_stdout.contains("address=1 exists=true occupied=false role=reserve"));
    assert!(client_stdout.contains("address=2 exists=true occupied=true role=circulation"));
    assert!(client_stdout.contains("address=3 exists=true occupied=true role=circulation"));

    let lower = client_stdout.to_ascii_lowercase();
    assert!(!lower.contains("owner"));
    assert!(!lower.contains("balance"));
    assert!(!client_stdout.contains("876543"));

    let status = server.wait().unwrap();
    assert!(status.success());

    store.remove_files().unwrap();
}
