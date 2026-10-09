use std::fs;
use std::process::Command;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use second::{SecondState, StateStore, ValidatorSetTransitionSource};
use serde_json::json;

use crate::support::{temp_base, validator_set};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_builds_verified_admission_transition_from_delta_plan() {
    let executable = env!("CARGO_BIN_EXE_second");
    let base = temp_base("cli-governance");
    let store = StateStore::new(&base);
    let current = validator_set(7, 1..=4);
    let baseline_account = crate::support::account(211);
    let pending_account = crate::support::account(212);
    let mut state = SecondState::genesis([baseline_account], 1);
    store.initialize(&state, &current).unwrap();
    let pending = crate::support::verified_task(
        7302,
        vec![second::Operation::RegisterAccount {
            account: pending_account,
        }],
    );
    second::PreparedTaskBook::new(store.clone())
        .unwrap()
        .prepare(&mut state, &pending, 1, &current)
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

    let transition = store.prepare_validator_set_transition(transition).unwrap();
    let statement = transition.finality_statement();
    let certificate = crate::support::certificate_from_keys(
        statement,
        &current,
        (1..=3).map(|id| {
            (
                second::ValidatorId::new(id),
                crate::support::key((id * 3 + 1) as u8),
            )
        }),
    );
    let certified = second::CertifiedValidatorSetTransition::new(
        transition,
        certificate.votes().to_vec(),
        &current,
    )
    .unwrap();
    store.activate_validator_set_transition(&certified).unwrap();
    let runtime = Arc::new(crate::support::bind_validator_runtime(
        &store,
        second::ValidatorRuntimeKeys::new(
            second::ValidatorId::new(1),
            crate::support::key(3),
            crate::support::key(4),
        ),
        crate::support::default_validator_runtime_config(),
    ));
    let running = Arc::clone(&runtime);
    let worker = tokio::spawn(async move { running.run(&[]).await });
    let address = runtime.local_addr().unwrap().to_string();
    let certificate = STANDARD.encode(runtime.transport_certificate_der());
    let trust_base = temp_base("cli-handoff-trust");
    let trust = StateStore::new(&trust_base);
    trust
        .initialize(&SecondState::genesis([], 1), &current)
        .unwrap();
    let destination_base = temp_base("cli-handoff-new-member");
    let destination = StateStore::new(&destination_base);
    let destination_keyring =
        crate::support::append_suffix(&destination_base, ".validator.keys.json");
    let destination_config = crate::support::append_suffix(&destination_base, ".validator.json");
    fs::copy(&keyring, &destination_keyring).unwrap();
    let config = |signer| {
        serde_json::to_vec(&json!({
        "authorizer_public_keys_base64": [STANDARD.encode(crate::support::key(signer).verifying_key().to_bytes())],
        "network_id_base64": STANDARD.encode([0_u8;32]),
        "bft_timeouts_ms": {"proposal": 1000, "prevote": 1000, "precommit": 1000}
    })).unwrap()
    };
    let command = || {
        vec![
            "handoff-install".to_owned(),
            address.clone(),
            destination_base.to_str().unwrap().to_owned(),
            trust_base.to_str().unwrap().to_owned(),
            certificate.clone(),
        ]
    };
    // Identity membership alone cannot authorize the task signatures in a body.
    fs::write(&destination_config, config(8)).unwrap();
    let args = command();
    let rejected =
        tokio::task::spawn_blocking(move || Command::new(executable).args(args).output().unwrap())
            .await
            .unwrap();
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("failed to install handoff baseline")
    );
    assert!(destination.load().unwrap().is_none());
    fs::write(&destination_config, config(9)).unwrap();
    let args = command();
    let installed =
        tokio::task::spawn_blocking(move || Command::new(executable).args(args).output().unwrap())
            .await
            .unwrap();
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    assert!(
        String::from_utf8_lossy(&installed.stdout)
            .contains("HANDOFF-INSTALLED validator=5 validator_set=8 safety=locked")
    );
    let snapshot = destination.load().unwrap().unwrap();
    assert_eq!(
        snapshot.validator_set,
        *certified.transition().next_validator_set()
    );
    assert!(snapshot.state.has_account(baseline_account));
    assert!(!snapshot.state.has_account(pending_account));
    assert_eq!(
        snapshot.state.bound_request_digest(pending.task_id()),
        Some(pending.request_digest())
    );
    assert!(!snapshot.validator_safety_ready);
    assert!(
        !second::PreparedTaskBook::new(destination.clone())
            .unwrap()
            .is_prepared(pending.task_id())
    );
    let generation = snapshot.generation;
    let args = command();
    let repeated =
        tokio::task::spawn_blocking(move || Command::new(executable).args(args).output().unwrap())
            .await
            .unwrap();
    assert!(!repeated.status.success());
    assert!(String::from_utf8_lossy(&repeated.stderr).contains("already initialized"));
    assert_eq!(destination.load().unwrap().unwrap().generation, generation);
    let base_arg = destination_base.to_str().unwrap().to_owned();
    let checked = tokio::task::spawn_blocking(move || {
        Command::new(executable)
            .args(["node-check", "127.0.0.1:0", &base_arg])
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    assert!(String::from_utf8_lossy(&checked.stdout).contains("VALIDATOR 5"));
    assert!(!destination.load().unwrap().unwrap().validator_safety_ready);
    worker.abort();
    let _ = worker.await;
    drop(runtime);
    crate::support::cleanup_node_runtime(store.clone(), base.clone());
    crate::support::cleanup_node_runtime(destination, destination_base);
    trust.remove_files().unwrap();
    fs::remove_file(destination_keyring).unwrap();
    fs::remove_file(destination_config).unwrap();

    for path in [keyring, admission, plan, source_path] {
        fs::remove_file(path).unwrap();
    }
}
