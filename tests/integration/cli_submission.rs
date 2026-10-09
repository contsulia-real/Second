use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::cli_network_init::RunningNode;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use second::{PublicStateStore, SecondState, StateStore};

use crate::support::{
    self, account, key, single_validator_set, temp_base, write_issue_transaction_request,
    write_validator_sidecars,
};

#[test]
fn failed_cli_fixture_releases_node_lock_and_preserves_snapshot() {
    let base = temp_base("cli-submit-panic-cleanup");
    let store = StateStore::new(&base);
    let initial = SecondState::genesis([account(77)], 1);
    let validators = single_validator_set(1, 1, 31, 32, 33);
    store.initialize(&initial, &validators).unwrap();
    let original = second::StateRecoveryPayload::from_persisted(&store.load().unwrap().unwrap())
        .unwrap()
        .encode_bytes()
        .unwrap();
    let (config, keyring) = write_validator_sidecars(
        &base,
        1,
        31,
        &[32],
        33,
        &[key(9).verifying_key().to_bytes()],
    );
    let failed = std::panic::catch_unwind(|| {
        let (_node, _, _) = start_node(&base);
        panic!("intentional CLI fixture failure");
    });
    assert!(failed.is_err());
    let check = Command::new(env!("CARGO_BIN_EXE_second"))
        .args(["node-check", "127.0.0.1:0", base.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "failed fixture leaked its running node: {}",
        String::from_utf8_lossy(&check.stderr)
    );
    let cold = StateStore::new(&base).load().unwrap().unwrap();
    assert_eq!(
        second::StateRecoveryPayload::from_persisted(&cold)
            .unwrap()
            .encode_bytes()
            .unwrap(),
        original
    );
    assert_eq!(cold.validator_set, validators);
    support::cleanup_node_runtime(store, base);
    fs::remove_file(config).unwrap();
    fs::remove_file(keyring).unwrap();
}

#[test]
fn validator_node_accepts_external_submission_commits_and_replays_idempotently() {
    let base = temp_base("cli-submit-validator");
    let store = StateStore::new(&base);
    let recipient = account(77);
    store
        .initialize(
            &SecondState::genesis([recipient], 1),
            &single_validator_set(1, 1, 31, 32, 33),
        )
        .unwrap();

    let authorizer = key(9);
    let (config_path, keyring_path) = write_validator_sidecars(
        &base,
        1,
        31,
        &[32],
        33,
        &[authorizer.verifying_key().to_bytes()],
    );
    let request_path = base.with_extension("request.json");
    let submitted_task_id =
        write_issue_transaction_request(&request_path, recipient, 7001, &authorizer, 1);

    let (mut node, address, certificate) = start_node(&base);
    let authorizer_public_key = STANDARD.encode(authorizer.verifying_key().to_bytes());

    let alias = base
        .parent()
        .unwrap()
        .join(".")
        .join(base.file_name().unwrap());
    for args in [
        vec!["node", "127.0.0.1:0", alias.to_str().unwrap()],
        vec![
            "public-init",
            alias.to_str().unwrap(),
            base.to_str().unwrap(),
        ],
        vec![
            "recovery-install",
            address.as_str(),
            alias.to_str().unwrap(),
            base.to_str().unwrap(),
            certificate.as_str(),
        ],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_second"))
            .args(args)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("node directory is already in use")
        );
    }

    let first = submit(
        &address,
        &request_path,
        &authorizer_public_key,
        &certificate,
    );
    if !first.status.success() {
        stop_node(&mut node);
        panic!(
            "external LegalTask submission failed: {}",
            String::from_utf8_lossy(&first.stderr)
        );
    }
    let first_stdout = String::from_utf8(first.stdout).unwrap();
    assert_eq!(
        first_stdout.trim(),
        format!("ACCEPTED task={submitted_task_id} state=allocating")
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let persisted = store.load().unwrap().unwrap();
        if persisted.state.current_supply() == 1 {
            assert_eq!(persisted.state.balance(recipient), 1);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "single-validator submitted task did not durably commit"
        );
        thread::sleep(Duration::from_millis(10));
    }

    let checkpoint = Command::new(env!("CARGO_BIN_EXE_second"))
        .args([
            "public-checkpoint",
            &address,
            base.to_str().unwrap(),
            &certificate,
        ])
        .output()
        .unwrap();
    assert!(
        checkpoint.status.success(),
        "{}",
        String::from_utf8_lossy(&checkpoint.stderr)
    );
    assert!(
        String::from_utf8(checkpoint.stdout)
            .unwrap()
            .contains("PUBLIC-CHECKPOINT-ACCEPTED validator_set=1 epoch=1")
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(proof) = store.load().unwrap().unwrap().public_checkpoint_proof {
            proof
                .verify_checkpoint(&store.load().unwrap().unwrap().validator_set)
                .unwrap();
            break;
        }
        assert!(
            Instant::now() < deadline,
            "CLI checkpoint did not persist a certified proof"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let certified = Command::new(env!("CARGO_BIN_EXE_second"))
        .args([
            "sync-public-certified",
            &address,
            base.to_str().unwrap(),
            &certificate,
        ])
        .output()
        .unwrap();
    assert!(
        certified.status.success(),
        "{}",
        String::from_utf8_lossy(&certified.stderr)
    );
    assert!(
        String::from_utf8(certified.stdout)
            .unwrap()
            .contains("supply=1")
    );

    let public = Command::new(env!("CARGO_BIN_EXE_second"))
        .args(["sync-public", address.as_str(), certificate.as_str()])
        .output()
        .unwrap();
    if !public.status.success() {
        stop_node(&mut node);
        panic!(
            "public state observation after submission failed: {}",
            String::from_utf8_lossy(&public.stderr)
        );
    }
    let public_stdout = String::from_utf8(public.stdout).unwrap();
    assert!(public_stdout.contains("supply=1"));
    assert!(public_stdout.contains("occupied=1"));

    let replay = submit(
        &address,
        &request_path,
        &authorizer_public_key,
        &certificate,
    );
    if !replay.status.success() {
        stop_node(&mut node);
        panic!(
            "idempotent completed LegalTask replay failed: {}",
            String::from_utf8_lossy(&replay.stderr)
        );
    }
    let replay_stdout = String::from_utf8(replay.stdout).unwrap();
    assert_eq!(
        replay_stdout.trim(),
        format!("ACCEPTED task={submitted_task_id} state=succeeded")
    );

    let status = task_status(
        &address,
        &request_path,
        &authorizer_public_key,
        &certificate,
    );
    if !status.status.success() {
        stop_node(&mut node);
        panic!(
            "LegalTask status query failed: {}",
            String::from_utf8_lossy(&status.stderr)
        );
    }
    assert_eq!(
        String::from_utf8(status.stdout).unwrap().trim(),
        format!("TASK task={submitted_task_id} state=succeeded")
    );

    let mismatched_request_path = base.with_extension("mismatched-request.json");
    let mismatched_authorizer = key(10);
    let mismatched_task_id = write_issue_transaction_request(
        &mismatched_request_path,
        recipient,
        7001,
        &mismatched_authorizer,
        2,
    );
    assert_eq!(mismatched_task_id, submitted_task_id);
    let mismatched_status = task_status(
        &address,
        &mismatched_request_path,
        &STANDARD.encode(mismatched_authorizer.verifying_key().to_bytes()),
        &certificate,
    );
    if !mismatched_status.status.success() {
        stop_node(&mut node);
        panic!(
            "mismatched LegalTask status query failed instead of returning unknown: {}",
            String::from_utf8_lossy(&mismatched_status.stderr)
        );
    }
    assert_eq!(
        String::from_utf8(mismatched_status.stdout).unwrap().trim(),
        format!("TASK task={submitted_task_id} state=unknown")
    );

    crate::cli_node_recovery::verify_checkpoint_install(
        &address,
        &certificate,
        &base,
        &keyring_path,
    );
    stop_node(&mut node);
    let (mut restarted, restart_address, restart_certificate) = start_node(&base);
    assert_eq!(restart_certificate, certificate);
    assert!(
        String::from_utf8_lossy(
            &task_status(
                &restart_address,
                &request_path,
                &authorizer_public_key,
                &restart_certificate
            )
            .stdout
        )
        .contains("state=succeeded")
    );
    stop_node(&mut restarted);
    support::cleanup_node_runtime(store, base);
    fs::remove_file(config_path).unwrap();
    fs::remove_file(keyring_path).unwrap();
    fs::remove_file(request_path).unwrap();
    fs::remove_file(mismatched_request_path).unwrap();
}

#[test]
fn public_only_node_rejects_external_submission() {
    let base = temp_base("cli-submit-public-only");
    let store = StateStore::new(&base);
    let recipient = account(88);
    store
        .initialize(
            &SecondState::genesis([recipient], 1),
            &single_validator_set(1, 1, 31, 32, 33),
        )
        .unwrap();

    let authorizer = key(9);
    let request_path = base.with_extension("request.json");
    write_issue_transaction_request(&request_path, recipient, 7002, &authorizer, 1);

    let (mut node, address, certificate) = start_node(&base);
    let output = submit(
        &address,
        &request_path,
        &STANDARD.encode(authorizer.verifying_key().to_bytes()),
        &certificate,
    );

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("target node does not provide LegalTask submission"));

    let status = task_status(
        &address,
        &request_path,
        &STANDARD.encode(authorizer.verifying_key().to_bytes()),
        &certificate,
    );
    assert!(status.status.success());
    let expected_task = support::task_id(7002);
    assert_eq!(
        String::from_utf8(status.stdout).unwrap().trim(),
        format!("TASK task={expected_task} state=unknown")
    );

    stop_node(&mut node);
    support::cleanup_node_runtime(store, base);
    fs::remove_file(request_path).unwrap();
}

#[test]
fn public_backend_rejects_private_task_status_query() {
    let trust_base = temp_base("cli-task-status-trust");
    let trust_store = StateStore::new(&trust_base);
    let recipient = account(99);
    trust_store
        .initialize(
            &SecondState::genesis([recipient], 1),
            &single_validator_set(1, 1, 31, 32, 33),
        )
        .unwrap();

    let public_base = temp_base("cli-task-status-public");
    let initialized = Command::new(env!("CARGO_BIN_EXE_second"))
        .args([
            "public-init",
            public_base.to_str().unwrap(),
            trust_base.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        initialized.status.success(),
        "public-init failed: {}",
        String::from_utf8_lossy(&initialized.stderr)
    );

    let authorizer = key(9);
    let request_path = public_base.with_extension("request.json");
    write_issue_transaction_request(&request_path, recipient, 7003, &authorizer, 1);

    let (mut node, address, certificate) = start_node(&public_base);
    let status = task_status(
        &address,
        &request_path,
        &STANDARD.encode(authorizer.verifying_key().to_bytes()),
        &certificate,
    );
    assert!(!status.status.success());
    assert!(
        String::from_utf8(status.stderr)
            .unwrap()
            .contains("target node does not provide private LegalTask status")
    );

    stop_node(&mut node);
    support::cleanup_public_node_runtime(PublicStateStore::new(&public_base), public_base);
    support::cleanup_node_runtime(trust_store, trust_base);
    fs::remove_file(request_path).unwrap();
}

fn start_node(base: &Path) -> (RunningNode, String, String) {
    let child = Command::new(env!("CARGO_BIN_EXE_second"))
        .args(["node", "127.0.0.1:0", base.to_str().unwrap()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut node = RunningNode {
        child,
        address: String::new(),
        certificate: String::new(),
        node_id: String::new(),
    };
    let stdout = node.child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);
    let mut listening = String::new();
    reader.read_line(&mut listening).unwrap();
    let fields = listening.split_whitespace().collect::<Vec<_>>();
    assert!(
        fields.len() >= 6 && fields[0] == "LISTENING" && fields[2] == "NODE" && fields[4] == "CERT",
        "unexpected node startup line: {listening}"
    );
    node.address = fields[1].to_owned();
    node.node_id = fields[3].to_owned();
    node.certificate = fields[5].to_owned();
    let address = node.address.clone();
    let certificate = node.certificate.clone();
    (node, address, certificate)
}

fn submit(
    address: &str,
    request_path: &Path,
    authorizer_public_key: &str,
    certificate: &str,
) -> Output {
    Command::new(env!("CARGO_BIN_EXE_second"))
        .args([
            "submit",
            address,
            request_path.to_str().unwrap(),
            authorizer_public_key,
            certificate,
        ])
        .output()
        .unwrap()
}

fn task_status(
    address: &str,
    request_path: &Path,
    authorizer_public_key: &str,
    certificate: &str,
) -> Output {
    Command::new(env!("CARGO_BIN_EXE_second"))
        .args([
            "task-status",
            address,
            request_path.to_str().unwrap(),
            authorizer_public_key,
            certificate,
        ])
        .output()
        .unwrap()
}

fn stop_node(node: &mut RunningNode) {
    node.child.kill().expect("node exited before test shutdown");
    node.child.wait().unwrap();
}
