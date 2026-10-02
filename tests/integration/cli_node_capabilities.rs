use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::json;

use crate::support;
use second::{SecondState, StateStore, ValidatorCredential, ValidatorId, ValidatorSet};
use support::{key, temp_base};

fn validator_set() -> ValidatorSet {
    ValidatorSet::new(
        1,
        [ValidatorCredential::new(
            ValidatorId::new(1),
            key(31).verifying_key().to_bytes(),
            key(32).verifying_key().to_bytes(),
            key(33).verifying_key().to_bytes(),
        )
        .unwrap()],
    )
    .unwrap()
}

fn write_validator_files(base: &Path, identity_seed: u8) -> (PathBuf, PathBuf) {
    let config_path = append_suffix(base, ".validator.json");
    let keyring_path = append_suffix(base, ".validator.keys.json");

    let config = json!({
        "authorizer_public_keys_base64": [
            STANDARD.encode(key(9).verifying_key().to_bytes())
        ],
        "bft_timeouts_ms": {
            "proposal": 250,
            "prevote": 250,
            "precommit": 250
        }
    });
    fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();

    let keyring = json!({
        "validator_id": 1,
        "identity_private_key_base64": STANDARD.encode(key(identity_seed).to_bytes()),
        "recovery_private_key_base64": STANDARD.encode(key(33).to_bytes()),
        "consensus_private_keys_base64": [
            STANDARD.encode(key(32).to_bytes())
        ]
    });
    fs::write(&keyring_path, serde_json::to_vec(&keyring).unwrap()).unwrap();
    secure_keyring_permissions(&keyring_path);

    (config_path, keyring_path)
}

#[test]
fn node_enables_validator_capability_when_sidecars_are_complete() {
    let base = temp_base("node-validator-capability");
    let store = StateStore::new(&base);
    store
        .initialize(&SecondState::genesis([], 1), &validator_set())
        .unwrap();
    let (config_path, keyring_path) = write_validator_files(&base, 31);

    let executable = env!("CARGO_BIN_EXE_second");
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
        .initialize(&SecondState::genesis([], 1), &validator_set())
        .unwrap();
    let (config_path, keyring_path) = write_validator_files(&base, 41);

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
        .initialize(&SecondState::genesis([], 1), &validator_set())
        .unwrap();
    let (config_path, keyring_path) = write_validator_files(&base, 31);
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

fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = OsString::from(path.as_os_str());
    value.push(suffix);
    PathBuf::from(value)
}

#[cfg(unix)]
fn secure_keyring_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

#[cfg(not(unix))]
fn secure_keyring_permissions(_path: &Path) {}
