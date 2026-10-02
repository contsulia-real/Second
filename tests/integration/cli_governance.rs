use std::fs;
use std::process::Command;

use second::{SecondState, StateStore, ValidatorSetTransitionSource};
use serde_json::json;

use crate::support::{temp_base, validator_set};

#[test]
fn cli_builds_verified_admission_transition_from_delta_plan() {
    let executable = env!("CARGO_BIN_EXE_second");
    let base = temp_base("cli-governance");
    let store = StateStore::new(&base);
    let current = validator_set(7, 1..=4);
    store
        .initialize(&SecondState::genesis([], 1), &current)
        .unwrap();

    let keyring = base.with_extension("candidate.keys.json");
    let admission = base.with_extension("candidate.admission");
    let plan = base.with_extension("transition.json");
    let source_path = base.with_extension("transition.source");

    let keygen = Command::new(executable)
        .args(["validator-keygen", "5", keyring.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        keygen.status.success(),
        "validator-keygen failed: {}",
        String::from_utf8_lossy(&keygen.stderr)
    );

    let admission_output = Command::new(executable)
        .args([
            "validator-admission",
            keyring.to_str().unwrap(),
            admission.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        admission_output.status.success(),
        "validator-admission failed: {}",
        String::from_utf8_lossy(&admission_output.stderr)
    );

    fs::write(
        &plan,
        serde_json::to_vec_pretty(&json!({
            "admission_request_files": [
                admission.file_name().unwrap().to_str().unwrap()
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let build = Command::new(executable)
        .args([
            "validator-transition-build",
            base.to_str().unwrap(),
            plan.to_str().unwrap(),
            source_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "validator-transition-build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let persisted = store.load().unwrap().unwrap();
    let source_bytes = fs::read(&source_path).unwrap();
    let source = ValidatorSetTransitionSource::decode_bytes(&source_bytes).unwrap();
    let transition = source
        .verify(&persisted.validator_set, &persisted.validator_registry)
        .unwrap();
    assert_eq!(transition.current_validator_set_version(), 7);
    assert_eq!(transition.next_validator_set().version(), 8);
    assert_eq!(transition.next_validator_set().len(), 5);
    assert!(
        transition
            .next_validator_set()
            .contains(second::ValidatorId::new(5))
    );
    assert_eq!(transition.admissions().len(), 1);

    store.remove_files().unwrap();
    for path in [keyring, admission, plan, source_path] {
        fs::remove_file(path).unwrap();
    }
}
