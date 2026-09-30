use std::fs;
use std::process::Command;

use crate::support;

use second::{
    AuthorizerSet, CURRENT_PROTOCOL_VERSION, LegalTask, LegalTaskPayload, Operation, SecondState,
    StateStore, ValidatorCredential, ValidatorId, ValidatorSet,
};
use support::{key, temp_base};

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

fn state_with_issue() -> SecondState {
    let alice = support::account(123456);
    let signing = key(9);
    let authorizers = AuthorizerSet::new(
        CURRENT_PROTOCOL_VERSION,
        [signing.verifying_key().to_bytes()],
    )
    .unwrap();
    let task = LegalTask::sign(
        LegalTaskPayload::new(
            support::task_id(1),
            CURRENT_PROTOCOL_VERSION,
            None,
            vec![Operation::Issue {
                account: alice,
                count: 2,
            }],
        ),
        &signing,
    )
    .unwrap()
    .verify(&authorizers)
    .unwrap();

    let mut state = SecondState::genesis([alice], 1);
    state.execute(&task, 1).unwrap();
    state
}

#[test]
fn snapshot_status_reports_only_public_safe_summary() {
    let base = temp_base("status");
    let store = StateStore::new(&base);
    store.save(&state_with_issue(), &validators()).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_second"))
        .arg("snapshot-status")
        .arg(&base)
        .output()
        .unwrap();

    assert!(output.status.success());

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("generation=1"));
    assert!(stdout.contains("supply=2"));
    assert!(stdout.contains("reserve=0"));
    assert!(stdout.contains("next_currency=3"));
    assert!(stdout.contains("validator_set=7"));
    assert!(stdout.contains("validators=4"));
    assert!(stdout.contains("quorum=3"));

    let lowercase = stdout.to_ascii_lowercase();
    assert!(!lowercase.contains("owner"));
    assert!(!lowercase.contains("balance"));
    assert!(!stdout.contains("123456"));

    store.remove_files().unwrap();
}

#[test]
fn snapshot_status_refuses_corrupted_state_instead_of_starting_fresh() {
    let base = temp_base("corrupt");
    let store = StateStore::new(&base);
    store.save(&state_with_issue(), &validators()).unwrap();

    fs::write(store.slot_path_for_generation(1), b"broken-a").unwrap();
    fs::write(store.slot_path_for_generation(2), b"broken-b").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_second"))
        .arg("snapshot-status")
        .arg(&base)
        .output()
        .unwrap();

    assert!(!output.status.success());

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("NoValidSnapshot"));

    store.remove_files().unwrap();
}
