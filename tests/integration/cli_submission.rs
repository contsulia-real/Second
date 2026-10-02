use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use second::{
    CURRENT_PROTOCOL_VERSION, LegalTask, LegalTaskPayload, Operation, SecondState, StateStore,
};
use serde_json::json;

use crate::support::{
    self, account, key, single_validator_set, task_id, temp_base, write_validator_sidecars,
};

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
    let submitted_task_id = write_issue_request(&request_path, recipient, 7001, &authorizer);

    let (mut node, address, certificate) = start_node(&base);
    let authorizer_public_key = STANDARD.encode(authorizer.verifying_key().to_bytes());

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
        format!("ACCEPTED task={submitted_task_id} state=prepared")
    );

    let deadline = Instant::now() + Duration::from_secs(4);
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

    stop_node(&mut node);
    support::cleanup_node_runtime(store, base);
    fs::remove_file(config_path).unwrap();
    fs::remove_file(keyring_path).unwrap();
    fs::remove_file(request_path).unwrap();
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
    write_issue_request(&request_path, recipient, 7002, &authorizer);

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

    stop_node(&mut node);
    support::cleanup_node_runtime(store, base);
    fs::remove_file(request_path).unwrap();
}

fn write_issue_request(
    path: &Path,
    recipient: second::AccountAddress,
    task_number: u128,
    authorizer: &ed25519_dalek::SigningKey,
) -> second::TaskId {
    let task_id = task_id(task_number);
    let task = LegalTask::sign(
        LegalTaskPayload::new(
            task_id.clone(),
            CURRENT_PROTOCOL_VERSION,
            None,
            vec![Operation::Issue {
                account: recipient,
                count: 1,
            }],
        ),
        authorizer,
    )
    .unwrap();
    let request = json!({
        "request_id": task_id.as_str(),
        "version": CURRENT_PROTOCOL_VERSION,
        "expires_at": null,
        "operations": [{
            "type": "issue",
            "recipient": recipient.to_string(),
            "amount": 1
        }],
        "signature": task.signature_base64url()
    });
    fs::write(path, serde_json::to_vec(&request).unwrap()).unwrap();
    task_id
}

fn start_node(base: &Path) -> (Child, String, String) {
    let mut node = Command::new(env!("CARGO_BIN_EXE_second"))
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
    assert!(
        fields.len() >= 6 && fields[0] == "LISTENING" && fields[2] == "NODE" && fields[4] == "CERT",
        "unexpected node startup line: {listening}"
    );
    (node, fields[1].to_owned(), fields[5].to_owned())
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

fn stop_node(node: &mut Child) {
    node.kill().expect("node exited before test shutdown");
    node.wait().unwrap();
}
