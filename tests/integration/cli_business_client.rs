use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use second::{AuthorizerSet, NetworkMessage, parse_transaction_request_json};
use serde_json::{Value, json};

use crate::support::{self, account, key, payment, task_id, temp_base};

async fn cli(args: Vec<String>) -> Output {
    tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_second"))
            .args(args)
            .output()
            .unwrap()
    })
    .await
    .unwrap()
}

fn checked(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

async fn sign(root: &Path, id: u128, operations: Value) -> std::path::PathBuf {
    let input = root.with_extension(format!("{id}.unsigned.json"));
    let output = root.with_extension(format!("{id}.signed.json"));
    fs::write(
        &input,
        serde_json::to_vec(&json!({
            "request_id": task_id(id).as_str(), "version": 1,
            "expires_at": null, "operations": operations
        }))
        .unwrap(),
    )
    .unwrap();
    checked(
        cli(vec![
            "transaction-sign".into(),
            root.with_extension("key.json").display().to_string(),
            input.display().to_string(),
            output.display().to_string(),
        ])
        .await,
    );
    let mut signed: Value = serde_json::from_slice(&fs::read(&output).unwrap()).unwrap();
    let original =
        parse_transaction_request_json(&serde_json::to_vec(&signed).unwrap(), [0; 32]).unwrap();
    let intent = support::sign_task(original.payload().clone(), &key(9)).unwrap();
    signed["account_signatures"] = json!(intent.account_signatures().iter().map(|entry|
        json!({"account":entry.account.to_string(),"signature":base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(entry.signature)})
    ).collect::<Vec<_>>());
    fs::write(&output, serde_json::to_vec_pretty(&signed).unwrap()).unwrap();
    output
}

#[tokio::test]
async fn business_signing_cli_validates_input_and_preserves_existing_files() {
    let root = temp_base("business-signing");
    let key_path = root.with_extension("key.json");
    let generated = checked(
        cli(vec![
            "authorizer-keygen".into(),
            key_path.display().to_string(),
        ])
        .await,
    );
    let public: [u8; 32] = STANDARD
        .decode(
            generated
                .trim()
                .strip_prefix("AUTHORIZER public_key=")
                .unwrap(),
        )
        .unwrap()
        .try_into()
        .unwrap();
    let original_key = fs::read(&key_path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        let permissions = fs::metadata(&key_path).unwrap().permissions();
        assert_eq!(permissions.mode() & 0o777, 0o600);
        fs::set_permissions(&key_path, fs::Permissions::from_mode(0o644)).unwrap();
        let rejected_output = root.with_extension("permissions-rejected.json");
        let rejected = cli(vec![
            "transaction-sign".into(),
            key_path.display().to_string(),
            "unused-input.json".into(),
            rejected_output.display().to_string(),
        ])
        .await;
        fs::set_permissions(&key_path, permissions).unwrap();
        assert!(!rejected.status.success());
        assert!(String::from_utf8_lossy(&rejected.stderr).contains("group/other"));
        assert!(!rejected_output.exists());
    }
    assert!(
        !cli(vec![
            "authorizer-keygen".into(),
            key_path.display().to_string()
        ])
        .await
        .status
        .success()
    );
    assert_eq!(fs::read(&key_path).unwrap(), original_key);
    let signed = sign(
        &root,
        8101,
        json!([{"type":"register_account","account":account(810).to_string()}]),
    )
    .await;
    let original = fs::read(&signed).unwrap();
    let task = parse_transaction_request_json(&original, public).unwrap();
    task.verify(&AuthorizerSet::new(1, [public]).unwrap())
        .unwrap();
    let input = root.with_extension("8101.unsigned.json");
    let sign_args = vec![
        "transaction-sign".into(),
        key_path.display().to_string(),
        input.display().to_string(),
        signed.display().to_string(),
    ];
    assert!(!cli(sign_args).await.status.success());
    assert_eq!(fs::read(&signed).unwrap(), original);
    let rejected = root.with_extension("rejected.json");
    for invalid in [
        json!({"request_id":task_id(8102).as_str(),"version":1,"operations":[{"type":"issue","recipient":account(810).to_string(),"amount":0}]}),
        json!({"request_id":task_id(8102).as_str(),"version":1,"operations":[{"type":"register_account","account":"wrong"}]}),
        serde_json::from_slice::<Value>(&original).unwrap(),
    ] {
        fs::write(&input, serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(
            !cli(vec![
                "transaction-sign".into(),
                key_path.display().to_string(),
                input.display().to_string(),
                rejected.display().to_string()
            ])
            .await
            .status
            .success()
        );
        assert!(!rejected.exists());
    }
    for path in [key_path, input, signed] {
        fs::remove_file(path).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn business_cli_cross_task_payment_survives_lost_ack_and_node_switch() {
    let root = temp_base("business-client");
    let key_path = root.with_extension("key.json");
    fs::write(
        &key_path,
        serde_json::to_vec(&json!({"private_key_base64": STANDARD.encode(key(9).to_bytes())}))
            .unwrap(),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let fixtures = (1..=4)
        .map(|id| support::validator_runtime_fixture("business-client-node", id))
        .collect::<Vec<_>>();
    let records = fixtures
        .iter()
        .map(|(runtime, _, _)| support::peer_record(runtime))
        .collect::<Vec<_>>();
    let workers = fixtures
        .iter()
        .enumerate()
        .map(|(index, (runtime, _, _))| {
            support::spawn_node_runtime_with_bootstrap(
                runtime,
                records
                    .iter()
                    .enumerate()
                    .filter(|(other, _)| *other != index)
                    .map(|(_, record)| record.clone())
                    .collect(),
            )
        })
        .collect::<Vec<_>>();
    tokio::time::timeout(Duration::from_secs(10), async {
        while fixtures
            .iter()
            .any(|(runtime, _, _)| runtime.connected_validator_ids().len() != 3)
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let alice = account(801);
    let bob = account(802);
    let source = payment(801);
    let destination = payment(802);
    let public_key = STANDARD.encode(key(9).verifying_key().to_bytes());
    let endpoint_args = |index: usize, request: &Path, command: &str| {
        vec![
            command.into(),
            fixtures[index].0.local_addr().unwrap().to_string(),
            request.display().to_string(),
            public_key.clone(),
            STANDARD.encode(fixtures[index].0.transport_certificate_der()),
        ]
    };
    let onboard = sign(&root, 8001, json!([
        {"type":"register_account","account":alice.to_string()},
        {"type":"register_account","account":bob.to_string()},
        {"type":"register_payment_address","address":source.to_string(),"account":alice.to_string()},
        {"type":"register_payment_address","address":destination.to_string(),"account":bob.to_string()}
    ])).await;
    checked(cli(endpoint_args(0, &onboard, "submit")).await);
    wait_success(&fixtures, 8001).await;
    let issued = sign(
        &root,
        8002,
        json!([{"type":"issue","recipient":alice.to_string(),"amount":2}]),
    )
    .await;

    // An actual QUIC relay forwards the complete request to Validator 1, then
    // withholds the accepted response. The caller times out after durable admission.
    let (relay, relay_certificate) = support::quic_server();
    let relay_address = relay.local_addr().unwrap();
    let backend_address = fixtures[0].0.local_addr().unwrap();
    let backend_certificate = fixtures[0].0.transport_certificate_der().to_vec();
    let relay_worker = tokio::spawn(async move {
        let frontend = relay.accept().await.unwrap();
        let backend = support::quic_client(&backend_certificate);
        let peer = backend.connect(backend_address).await.unwrap();
        loop {
            let request = frontend.accept_request().await.unwrap().unwrap();
            let response = peer.exchange(request.message()).await.unwrap();
            if matches!(response, NetworkMessage::LegalTaskSubmissionAccepted { .. }) {
                tokio::time::sleep(Duration::from_secs(7)).await;
                drop(request);
                break;
            }
            request.respond(&response).await.unwrap();
        }
    });
    let lost = cli(vec![
        "submit".into(),
        relay_address.to_string(),
        issued.display().to_string(),
        public_key.clone(),
        STANDARD.encode(&relay_certificate),
    ])
    .await;
    assert!(!lost.status.success());
    assert!(
        String::from_utf8_lossy(&lost.stderr).contains("timed out"),
        "{}",
        String::from_utf8_lossy(&lost.stderr)
    );
    wait_success(&fixtures, 8002).await;
    let replay = checked(cli(endpoint_args(2, &issued, "submit")).await);
    assert!(replay.contains("state=succeeded"));
    relay_worker.abort();
    let transfer = sign(&root, 8003, json!([{"type":"transfer","source":source.to_string(),"destination":destination.to_string(),"amount":1}])).await;
    checked(cli(endpoint_args(1, &transfer, "submit")).await);
    wait_success(&fixtures, 8003).await;
    assert!(
        checked(cli(endpoint_args(3, &transfer, "task-status")).await).contains("state=succeeded")
    );
    for (_, _, base) in &fixtures {
        let state = second::StateStore::new(base).load().unwrap().unwrap().state;
        assert_eq!(state.balance(alice), 1);
        assert_eq!(state.balance(bob), 1);
    }
    // Same id, separately valid signature, different business intent: reject.
    let conflict_root = root.with_file_name(format!(
        "{}-conflict",
        root.file_name().unwrap().to_string_lossy()
    ));
    fs::copy(&key_path, conflict_root.with_extension("key.json")).unwrap();
    let conflict = sign(&conflict_root, 8003, json!([{"type":"transfer","source":source.to_string(),"destination":destination.to_string(),"amount":2}])).await;
    assert!(
        !cli(endpoint_args(3, &conflict, "submit"))
            .await
            .status
            .success()
    );
    assert!(
        checked(cli(endpoint_args(3, &conflict, "task-status")).await).contains("state=unknown")
    );
    let mut tampered: Value = serde_json::from_slice(&fs::read(&transfer).unwrap()).unwrap();
    tampered["operations"][0]["amount"] = json!(2);
    let tampered_path = root.with_extension("tampered.json");
    fs::write(&tampered_path, serde_json::to_vec(&tampered).unwrap()).unwrap();
    assert!(
        !cli(endpoint_args(1, &tampered_path, "submit"))
            .await
            .status
            .success()
    );
    // Inject a certified terminal fixture without claiming to test initial arbitration.
    // The real CLI/QUIC status path must preserve cancellation and private digest matching.
    let cancelled = sign(
        &root,
        8005,
        json!([{"type":"register_account","account":account(805).to_string()}]),
    )
    .await;
    let public = key(9).verifying_key().to_bytes();
    let task = parse_transaction_request_json(&fs::read(&cancelled).unwrap(), public)
        .unwrap()
        .verify(&AuthorizerSet::new(1, [public]).unwrap())
        .unwrap();
    for (_, store, _) in &fixtures {
        let snapshot = store.load().unwrap().unwrap();
        let mut state = snapshot.state;
        let mut book = second::PreparedTaskBook::new(store.clone()).unwrap();
        book.prepare(&mut state, &task, 1, &snapshot.validator_set)
            .unwrap();
        let statement = store.prepared_abort_statement(&task.task_id()).unwrap();
        let certificate = support::certificate_from_keys(
            statement,
            &snapshot.validator_set,
            (1..=3).map(|id| (second::ValidatorId::new(id), key((id * 3 + 1) as u8))),
        );
        book.abort_certified(&mut state, task.task_id(), &certificate)
            .unwrap();
    }
    assert!(
        checked(cli(endpoint_args(3, &cancelled, "task-status")).await).contains("state=cancelled")
    );
    assert!(
        !cli(endpoint_args(1, &cancelled, "submit"))
            .await
            .status
            .success()
    );
    #[cfg(windows)]
    {
        let endpoints = root.with_extension("endpoints.json");
        fs::write(&endpoints, serde_json::to_vec(&json!([
            {"address":"127.0.0.1:0","certificate_base64":STANDARD.encode(fixtures[0].0.transport_certificate_der())},
            {"address":fixtures[3].0.local_addr().unwrap().to_string(),"certificate_base64":STANDARD.encode(fixtures[3].0.transport_certificate_der())}
        ])).unwrap()).unwrap();
        let request = sign(
            &root,
            8004,
            json!([{ "type": "retire_payment_address", "address": source.to_string() }]),
        )
        .await;
        let pubkey = public_key.clone();
        let result = tokio::task::spawn_blocking(move || {
            Command::new("pwsh")
                .args([
                    "-NoProfile",
                    "-File",
                    "examples/submit-transaction.ps1",
                    "-SecondExe",
                    env!("CARGO_BIN_EXE_second"),
                    "-TransactionFile",
                    request.to_str().unwrap(),
                    "-AuthorizerPublicKey",
                    &pubkey,
                    "-EndpointsFile",
                    endpoints.to_str().unwrap(),
                    "-MaxAttempts",
                    "4",
                ])
                .output()
                .unwrap()
        })
        .await
        .unwrap();
        assert!(checked(result).contains("state=succeeded"));
        wait_success(&fixtures, 8004).await;
        for (_, store, _) in &fixtures {
            assert_eq!(
                store
                    .load()
                    .unwrap()
                    .unwrap()
                    .state
                    .payment_address_status(source),
                Some(second::PaymentAddressStatus::Retiring)
            );
        }
        let cancelled_args = endpoint_args(3, &cancelled, "task-status");
        let cancelled_endpoints = root.with_extension("cancelled-endpoints.json");
        fs::write(
            &cancelled_endpoints,
            serde_json::to_vec(&json!([
                {"address":cancelled_args[1], "certificate_base64":cancelled_args[4]}
            ]))
            .unwrap(),
        )
        .unwrap();
        let result = tokio::task::spawn_blocking(move || {
            Command::new("pwsh")
                .args([
                    "-NoProfile",
                    "-File",
                    "examples/submit-transaction.ps1",
                    "-SecondExe",
                    env!("CARGO_BIN_EXE_second"),
                    "-TransactionFile",
                    cancelled.to_str().unwrap(),
                    "-AuthorizerPublicKey",
                    &public_key,
                    "-EndpointsFile",
                    cancelled_endpoints.to_str().unwrap(),
                    "-MaxAttempts",
                    "2",
                ])
                .output()
                .unwrap()
        })
        .await
        .unwrap();
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("state=cancelled"));
        fs::remove_file(root.with_extension("cancelled-endpoints.json")).unwrap();
        fs::remove_file(root.with_extension("8004.unsigned.json")).unwrap();
        fs::remove_file(root.with_extension("8004.signed.json")).unwrap();
    }
    for (worker, (runtime, store, base)) in workers.into_iter().zip(fixtures) {
        worker.abort();
        let _ = worker.await;
        drop(runtime);
        support::cleanup_node_runtime(store, base);
    }
    for (base, ids) in [
        (&root, vec![8001, 8002, 8003, 8005]),
        (&conflict_root, vec![8003]),
    ] {
        for id in ids {
            fs::remove_file(base.with_extension(format!("{id}.unsigned.json"))).unwrap();
            fs::remove_file(base.with_extension(format!("{id}.signed.json"))).unwrap();
        }
        fs::remove_file(base.with_extension("key.json")).unwrap();
    }
    fs::remove_file(tampered_path).unwrap();
    #[cfg(windows)]
    fs::remove_file(root.with_extension("endpoints.json")).unwrap();
}

async fn wait_success(
    fixtures: &[(
        std::sync::Arc<second::NodeRuntime>,
        second::StateStore,
        std::path::PathBuf,
    )],
    id: u128,
) {
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if fixtures.iter().all(|(_, store, _)| {
                store
                    .load()
                    .unwrap()
                    .unwrap()
                    .state
                    .task_succeeded(task_id(id))
                    == Some(true)
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    if result.is_err() {
        for (index, (runtime, store, base)) in fixtures.iter().enumerate() {
            let snapshot = store.load().unwrap().unwrap();
            eprintln!(
                "task={id} node={} base={} peers={:?} succeeded={:?} cancelled={} bft={:?} events={:?}",
                index + 1,
                base.display(),
                runtime.connected_validator_ids(),
                snapshot.state.task_succeeded(task_id(id)),
                snapshot.state.task_cancelled(task_id(id)),
                store
                    .bft_local_state(
                        second::ValidatorId::new(index as u64 + 1),
                        &second::ConsensusScope::PreparedTask(task_id(id))
                    )
                    .unwrap(),
                runtime.drain_bft_consensus_events()
            );
        }
    }
    assert!(
        result.is_ok(),
        "task {id} did not reach success at every validator"
    );
}
