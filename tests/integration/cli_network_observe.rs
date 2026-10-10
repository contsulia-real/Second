use std::fs;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

use crate::support;
use second::{
    CURRENT_PROTOCOL_VERSION, PublicCurrencyCheckpoint, PublicCurrencyCheckpointProof, SecondState,
    StateStore, ValidatorId,
};
use support::{key, signed_vote, temp_base, validator_set};

fn checkpoint_proof(state: &SecondState, epoch: u64) -> PublicCurrencyCheckpointProof {
    let validators = validator_set(1, 1..=4);
    let checkpoint = PublicCurrencyCheckpoint::new(
        CURRENT_PROTOCOL_VERSION,
        epoch,
        state.public_currency_summary(),
    );
    let statement = checkpoint.finality_statement(validators.version());
    let votes = [1_u64, 2, 3]
        .into_iter()
        .map(|id| signed_vote(&statement, ValidatorId::new(id), &key((id * 3 + 1) as u8)))
        .collect();

    PublicCurrencyCheckpointProof::new(checkpoint, validators.version(), votes)
}

fn parse_listening(line: &str) -> (String, String, String) {
    let mut fields = line.split_whitespace();
    assert_eq!(fields.next(), Some("LISTENING"));
    let address = fields.next().unwrap().to_owned();
    assert_eq!(fields.next(), Some("NODE"));
    let node_id = fields.next().unwrap().to_owned();
    assert_eq!(fields.next(), Some("CERT"));
    let certificate = fields.next().unwrap().to_owned();
    assert_eq!(fields.next(), None);
    (address, node_id, certificate)
}

#[test]
fn real_process_observes_certified_public_state_via_bootstrap_without_mutating_snapshot() {
    let seed_base = temp_base("observe-public-seed");
    let observer_base = temp_base("observe-public-observer");
    let seed_store = StateStore::new(&seed_base);
    let observer_store = StateStore::new(&observer_base);
    let validators = validator_set(1, 1..=4);

    let seed_state = SecondState::genesis([], 10).with_reserve(5).unwrap();
    seed_store.initialize(&seed_state, &validators).unwrap();
    seed_store
        .attach_checkpoint_proof(Some(&checkpoint_proof(&seed_state, 44)))
        .unwrap();

    let private_observer_state = SecondState::genesis([], 900).with_reserve(2).unwrap();
    observer_store
        .initialize(&private_observer_state, &validators)
        .unwrap();

    let executable = env!("CARGO_BIN_EXE_second");
    let mut seed = Command::new(executable)
        .args(["node", "127.0.0.1:0", seed_base.to_str().unwrap()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let mut reader = BufReader::new(seed.stdout.take().unwrap());
    let mut listening = String::new();
    reader.read_line(&mut listening).unwrap();
    let (seed_address, seed_node_id, seed_certificate) = parse_listening(&listening);

    let bootstrap = serde_json::json!([{
        "node_id": seed_node_id,
        "address": seed_address,
        "certificate_base64": seed_certificate,
    }]);
    fs::write(
        support::bootstrap_config_path(&observer_base),
        serde_json::to_vec_pretty(&bootstrap).unwrap(),
    )
    .unwrap();

    let observed = Command::new(executable)
        .args(["observe-public-network", observer_base.to_str().unwrap()])
        .output()
        .unwrap();

    if !observed.status.success() {
        let _ = seed.kill();
        let _ = seed.wait();
        panic!(
            "observe-public-network failed: {}",
            String::from_utf8_lossy(&observed.stderr)
        );
    }

    let stdout = String::from_utf8(observed.stdout).unwrap();
    assert!(stdout.contains("NETWORK-CERTIFIED "));
    assert!(stdout.contains(&format!("peer={seed_node_id}")));
    assert!(stdout.contains("epoch=44"));
    assert!(stdout.contains("validator_set=1"));
    assert!(stdout.contains("votes=3"));
    assert!(stdout.contains("count=1"));
    assert!(stdout.contains("supply=5"));
    assert!(stdout.contains("reserve=5"));
    assert!(stdout.contains("occupied=5"));
    assert!(stdout.contains("next_currency=15"));

    let persisted = observer_store.load().unwrap().unwrap();
    assert_eq!(persisted.state.next_currency_address(), 902);
    assert_eq!(persisted.checkpoint_floor_epoch, 0);
    assert!(persisted.public_checkpoint_proof.is_none());

    let lower = stdout.to_ascii_lowercase();
    assert!(!lower.contains("owner"));
    assert!(!lower.contains("balance"));

    seed.kill().expect("seed node exited before test shutdown");
    seed.wait().unwrap();

    support::cleanup_node_runtime(seed_store, seed_base);
    support::cleanup_node_runtime(observer_store, observer_base);
}
