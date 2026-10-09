use std::fs;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use crate::support;
use second::StateStore;

/// Reuse the live CLI submission fixture instead of starting a duplicate network.
pub(crate) fn verify_checkpoint_install(
    address: &str,
    certificate: &str,
    trust_base: &Path,
    keyring: &Path,
) {
    let executable = env!("CARGO_BIN_EXE_second");
    let accepted = Command::new(executable)
        .args([
            "recovery-checkpoint",
            address,
            trust_base.to_str().unwrap(),
            certificate,
        ])
        .output()
        .unwrap();
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stderr)
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let trusted = loop {
        let persisted = StateStore::new(trust_base).load().unwrap().unwrap();
        if persisted.recovery_checkpoint_proof.is_some() {
            break persisted;
        }
        assert!(
            Instant::now() < deadline,
            "CLI recovery checkpoint did not finalize"
        );
        thread::sleep(Duration::from_millis(20));
    };
    let destination = support::temp_base("cli-recovered-node");
    let destination_keyring = support::append_suffix(&destination, ".validator.keys.json");
    fs::copy(keyring, &destination_keyring).unwrap();
    let install = Command::new(executable)
        .args([
            "recovery-install",
            address,
            destination.to_str().unwrap(),
            trust_base.to_str().unwrap(),
            certificate,
        ])
        .output()
        .unwrap();
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    assert!(String::from_utf8_lossy(&install.stdout).contains("safety=locked"));
    let recovered = StateStore::new(&destination).load().unwrap().unwrap();
    assert!(!recovered.validator_safety_ready);
    assert_eq!(recovered.validator_set, trusted.validator_set);
    assert_eq!(
        recovered.state.current_supply(),
        trusted.state.current_supply()
    );
    assert_eq!(
        recovered.state.next_currency_address(),
        trusted.state.next_currency_address()
    );
    assert_eq!(
        second::StateRecoveryPayload::from_persisted(&recovered)
            .unwrap()
            .encode_bytes()
            .unwrap(),
        second::StateRecoveryPayload::from_persisted(&trusted)
            .unwrap()
            .encode_bytes()
            .unwrap()
    );
    let status = Command::new(executable)
        .args(["snapshot-status", destination.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(status.status.success());
    assert!(String::from_utf8_lossy(&status.stdout).contains("safety=locked"));
    let rejected = Command::new(executable)
        .args([
            "recovery-install",
            address,
            destination.to_str().unwrap(),
            trust_base.to_str().unwrap(),
            certificate,
        ])
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("already initialized"));
    let after = StateStore::new(&destination).load().unwrap().unwrap();
    assert_eq!(after.generation, recovered.generation);
    assert!(!after.validator_safety_ready);
    support::cleanup_node_runtime(StateStore::new(&destination), destination);
    fs::remove_file(destination_keyring).unwrap();
}
