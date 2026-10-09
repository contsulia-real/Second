use std::fs;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

use crate::support;
use second::{SecondState, StateStore};
use support::{key, single_validator_set, temp_base, write_validator_sidecars};

#[test]
fn node_enables_validator_capability_when_sidecars_are_complete() {
    let base = temp_base("node-validator-capability");
    let store = StateStore::new(&base);
    store
        .initialize(
            &SecondState::genesis([], 1),
            &single_validator_set(1, 1, 31, 32, 33),
        )
        .unwrap();
    let (config_path, keyring_path) = write_validator_sidecars(
        &base,
        1,
        31,
        &[32],
        33,
        &[key(9).verifying_key().to_bytes()],
    );

    let executable = env!("CARGO_BIN_EXE_second");
    let before_check = store.load().unwrap().unwrap().generation;
    let check = Command::new(executable)
        .args(["node-check", "127.0.0.1:0", base.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let checked = String::from_utf8(check.stdout).unwrap();
    let check_certificate = checked.split_whitespace().nth(6).unwrap();
    assert_eq!(store.load().unwrap().unwrap().generation, before_check);
    let mut node = Command::new(executable)
        .args(["node", "127.0.0.1:0", base.to_str().unwrap()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let stdout = node.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);
    let mut listening = String::new();
    reader.read_line(&mut listening).unwrap();

    let fields = listening.split_whitespace().collect::<Vec<_>>();
    assert_eq!(
        fields.len(),
        8,
        "unexpected validator-capable node startup line: {listening}"
    );
    assert_eq!(fields[0], "LISTENING");
    assert_eq!(fields[2], "NODE");
    assert_eq!(fields[4], "CERT");
    assert_eq!(fields[6], "VALIDATOR");
    assert_eq!(fields[7], "1");
    assert_eq!(fields[5], check_certificate);
    let busy_check = Command::new(executable)
        .args(["node-check", "127.0.0.1:0", base.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        !busy_check.status.success(),
        "preflight must refuse an active node base"
    );

    let ping = Command::new(executable)
        .args(["ping", fields[1], "77", fields[5]])
        .output()
        .unwrap();
    if !ping.status.success() {
        let _ = node.kill();
        let _ = node.wait();
        panic!(
            "validator-capable node public runtime ping failed: {}",
            String::from_utf8_lossy(&ping.stderr)
        );
    }
    let ping_stdout = String::from_utf8(ping.stdout).unwrap();
    assert!(ping_stdout.contains("PONG "));
    assert!(ping_stdout.contains("nonce=77"));

    node.kill().expect("node exited before test shutdown");
    node.wait().unwrap();

    support::cleanup_node_runtime(store, base);
    fs::remove_file(config_path).unwrap();
    fs::remove_file(keyring_path).unwrap();
}

#[test]
fn node_rejects_validator_keyring_that_does_not_match_registry_before_binding() {
    let base = temp_base("node-validator-bad-key");
    let store = StateStore::new(&base);
    store
        .initialize(
            &SecondState::genesis([], 1),
            &single_validator_set(1, 1, 31, 32, 33),
        )
        .unwrap();
    let (config_path, keyring_path) = write_validator_sidecars(
        &base,
        1,
        41,
        &[32],
        33,
        &[key(9).verifying_key().to_bytes()],
    );

    let output = Command::new(env!("CARGO_BIN_EXE_second"))
        .args(["node", "127.0.0.1:0", base.to_str().unwrap()])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("identity key does not match ValidatorId 1"));
    assert!(!support::transport_identity_path(&base).exists());

    store.remove_files().unwrap();
    fs::remove_file(config_path).unwrap();
    fs::remove_file(keyring_path).unwrap();
}

#[test]
fn node_rejects_partial_validator_capability_before_binding() {
    let base = temp_base("node-validator-partial");
    let store = StateStore::new(&base);
    store
        .initialize(
            &SecondState::genesis([], 1),
            &single_validator_set(1, 1, 31, 32, 33),
        )
        .unwrap();
    let (config_path, keyring_path) = write_validator_sidecars(
        &base,
        1,
        31,
        &[32],
        33,
        &[key(9).verifying_key().to_bytes()],
    );
    fs::remove_file(&keyring_path).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_second"))
        .args(["node", "127.0.0.1:0", base.to_str().unwrap()])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("validator capability is partially configured"));
    assert!(!support::transport_identity_path(&base).exists());

    store.remove_files().unwrap();
    fs::remove_file(config_path).unwrap();
}
