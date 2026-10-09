//! Required local acceptance: two native Windows and two native Linux validators.
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use second::{Operation, StateRecoveryPayload, StateStore};
use serde_json::{Value, json};

use crate::cli_network_init::start_node;
use crate::support::{self, account, key, payment, write_transaction_request_in_network};

mod soak;
mod wallet;

const M0_ACCOUNT_ASSETS: u64 = 257;

struct LinuxWorker {
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
}

impl LinuxWorker {
    fn request(&mut self, request: Value) -> Value {
        let input = self.input.as_mut().unwrap();
        writeln!(input, "{request}").unwrap();
        input.flush().unwrap();
        let mut line = String::new();
        self.output.read_line(&mut line).unwrap();
        let response: Value = serde_json::from_str(&line).expect("Linux worker response");
        assert!(response.get("error").is_none(), "{response}");
        response["result"].clone()
    }

    fn cli(&mut self, args: &[&str]) -> String {
        let result = self.request(json!({"action":"cli", "args":args}));
        assert_eq!(result["code"], 0, "{args:?}: {result}");
        result["stdout"].as_str().unwrap().to_owned()
    }

    fn start(&mut self, id: u64, address: &str, base: &str) -> Vec<String> {
        let line = self.request(json!({"action":"start", "id":id, "address":address, "base":base}));
        let fields = line
            .as_str()
            .unwrap()
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert_eq!(fields.len(), 8, "{line}");
        assert_eq!(fields[0], "LISTENING");
        assert_eq!(fields[1], address);
        assert_eq!(fields[6], "VALIDATOR");
        assert_eq!(fields[7], id.to_string());
        fields
    }

    fn stop(&mut self, id: u64) {
        self.request(json!({"action":"stop", "id":id}));
    }
}

impl Drop for LinuxWorker {
    fn drop(&mut self) {
        if let Some(mut input) = self.input.take() {
            let _ = writeln!(input, "{{\"action\":\"close\"}}");
            let _ = input.flush();
        }
        let _ = self.child.wait();
    }
}

fn cli(args: &[&str]) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let output = Command::new(env!("CARGO_BIN_EXE_second"))
            .args(args)
            .output()
            .unwrap();
        if output.status.success() {
            return String::from_utf8(output.stdout).unwrap();
        }
        let error = String::from_utf8_lossy(&output.stderr);
        if error.contains("GovernanceRejected(Busy)") && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(200));
            continue;
        }
        panic!("{args:?}: {error}");
    }
}

fn wsl_path(distro: &str, path: &Path) -> String {
    let output = Command::new("wsl.exe")
        .args([
            "-d",
            distro,
            "--exec",
            "wslpath",
            "-u",
            path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn wait(label: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(45);
    while !condition() {
        assert!(Instant::now() < deadline, "deadline: {label}");
        thread::sleep(Duration::from_millis(100));
    }
    println!("MIXED-PASS {label}");
}

struct ObservedSnapshot {
    state: second::SecondState,
    validator_set: second::ValidatorSet,
    recovery_checkpoint_proof: Option<bool>,
    validator_safety_ready: bool,
    minimum_signing_validator_set_version: u64,
    payload: Vec<u8>,
}

fn snapshot(base: &Path, worker: &mut LinuxWorker) -> ObservedSnapshot {
    if base.to_str().unwrap().starts_with('/') {
        let result = worker.request(json!({"action":"snapshot", "base":base.to_str().unwrap()}));
        let bytes = STANDARD
            .decode(result["payload"].as_str().unwrap())
            .unwrap();
        let shared = StateRecoveryPayload::decode_bytes(&bytes).unwrap();
        ObservedSnapshot {
            state: shared.state().clone(),
            validator_set: shared.validator_set().clone(),
            recovery_checkpoint_proof: result["recovery_proof"].as_bool().unwrap().then_some(true),
            validator_safety_ready: result["safety_ready"].as_bool().unwrap(),
            minimum_signing_validator_set_version: result["minimum_signing_version"]
                .as_u64()
                .unwrap(),
            payload: bytes,
        }
    } else {
        let persisted = StateStore::new(base).load().unwrap().unwrap();
        let payload = StateRecoveryPayload::from_persisted(&persisted)
            .unwrap()
            .encode_bytes()
            .unwrap();
        ObservedSnapshot {
            state: persisted.state,
            validator_set: persisted.validator_set,
            recovery_checkpoint_proof: persisted.recovery_checkpoint_proof.map(|_| true),
            validator_safety_ready: persisted.validator_safety_ready,
            minimum_signing_validator_set_version: persisted.minimum_signing_validator_set_version,
            payload,
        }
    }
}

fn equal_shared(bases: &[PathBuf], worker: &mut LinuxWorker) {
    let expected = snapshot(&bases[0], worker).payload;
    for base in &bases[1..] {
        assert_eq!(snapshot(base, worker).payload, expected);
    }
}

#[test]
fn mixed_windows_linux_validators_pay_and_recover_into_required_quorum() {
    let soak_seconds = std::env::var("SECOND_MIXED_SOAK_SECONDS")
        .map(|seconds| {
            seconds
                .parse::<u64>()
                .expect("soak seconds must be an integer")
        })
        .unwrap_or(0);
    assert!(soak_seconds <= 3600, "explicit soak is limited to one hour");
    let executable = env!("CARGO_BIN_EXE_second");
    let distro = std::env::var("SECOND_WSL_DISTRO").unwrap_or_else(|_| "Ubuntu-26.04".to_owned());
    let binary = std::env::var("SECOND_WSL_BINARY")
        .expect("set SECOND_WSL_BINARY to the native Linux second executable");
    let script = wsl_path(
        &distro,
        &std::env::current_dir()
            .unwrap()
            .join("tests/mixed_wsl_worker.py"),
    );
    let mut child = Command::new("wsl.exe")
        .args(["-d", &distro, "--exec", "python3", &script, &binary])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut worker = LinuxWorker {
        input: child.stdin.take(),
        output: BufReader::new(child.stdout.take().unwrap()),
        child,
    };
    let linux_root = worker
        .request(json!({"action":"info"}))
        .as_str()
        .unwrap()
        .to_owned();
    let root = support::temp_base("mixed-wsl");
    fs::create_dir(&root).unwrap();
    println!(
        "MIXED-ARTIFACTS windows={} linux={linux_root}",
        root.display()
    );
    let deployment = root.join("network");
    let config = root.join("network.json");
    let sockets = (0..4)
        .map(|_| UdpSocket::bind("127.0.0.1:0").unwrap())
        .collect::<Vec<_>>();
    let addresses = sockets
        .iter()
        .map(|s| s.local_addr().unwrap())
        .collect::<Vec<_>>();
    let address_strings = addresses
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let authorizer = key(9);
    let public_key = STANDARD.encode(authorizer.verifying_key().to_bytes());
    let mut validators = Vec::new();
    for id in 1..=4 {
        let key_file = root.join(format!("validator-{id}.keys.json"));
        cli(&[
            "validator-keygen",
            &id.to_string(),
            key_file.to_str().unwrap(),
        ]);
        validators.push(json!({"validator_id":id,"listen_address":address_strings[id-1],"keyring_file":key_file.file_name().unwrap().to_str().unwrap()}));
    }
    fs::write(&config, serde_json::to_vec_pretty(&json!({
        "validator_set_version":1,"first_currency_address":1,"reserve_count":0,"accounts":[],
        "authorizer_public_keys_base64":[public_key],
        "bft_timeouts_ms":{"proposal":1000,"prevote":1000,"precommit":1000},"validators":validators
    })).unwrap()).unwrap();
    cli(&[
        "init-network",
        config.to_str().unwrap(),
        deployment.to_str().unwrap(),
    ]);
    let wallet_config: Value =
        serde_json::from_slice(&fs::read(deployment.join("wallet-network.json")).unwrap()).unwrap();
    let network_id: [u8; 32] = STANDARD
        .decode(wallet_config["network_id_base64"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let linux_deployment = worker
        .request(json!({"action":"import","source":wsl_path(&distro,&deployment)}))
        .as_str()
        .unwrap()
        .to_owned();
    let linux_bases = (3..=4)
        .map(|id| format!("{linux_deployment}/validator-{id}/second"))
        .collect::<Vec<_>>();
    let mut bases = vec![
        deployment.join("validator-1/second"),
        deployment.join("validator-2/second"),
        PathBuf::from(&linux_bases[0]),
        PathBuf::from(&linux_bases[1]),
    ];
    drop(sockets);
    let mut windows = (0..2)
        .map(|i| {
            Some(start_node(
                executable,
                addresses[i],
                &bases[i],
                i as u64 + 1,
            ))
        })
        .collect::<Vec<_>>();
    let linux_three = worker.start(3, &address_strings[2], &linux_bases[0]);
    let linux_four = worker.start(4, &address_strings[3], &linux_bases[1]);
    let certs = [
        windows[0].as_ref().unwrap().certificate.clone(),
        windows[1].as_ref().unwrap().certificate.clone(),
        linux_three[5].clone(),
        linux_four[5].clone(),
    ];
    // Actual pinned QUIC in both directions. No proxy or TCP relay.
    cli(&["ping", &address_strings[2], "31", &certs[2]]);
    worker.cli(&["ping", &address_strings[0], "13", &certs[0]]);
    let wrong_certificate = Command::new(executable)
        .args(["ping", &address_strings[2], "99", &certs[0]])
        .output()
        .unwrap();
    assert!(
        !wrong_certificate.status.success(),
        "cross-system certificate mismatch was accepted"
    );
    println!("MIXED-PASS bidirectional pinned QUIC; wrong certificate rejected");
    thread::sleep(Duration::from_secs(3));
    let alice = account(7101);
    let bob = account(7102);
    let source = payment(7101);
    let destination = payment(7102);
    let request = root.join("business.json");
    let task = write_transaction_request_in_network(
        &request,
        7101,
        &authorizer,
        vec![
            Operation::RegisterAccount { account: alice },
            Operation::RegisterAccount { account: bob },
            Operation::RegisterPaymentAddress {
                address: source,
                account: alice,
            },
            Operation::RegisterPaymentAddress {
                address: destination,
                account: bob,
            },
            Operation::Issue {
                account: alice,
                count: M0_ACCOUNT_ASSETS + 1,
            },
            Operation::Transfer {
                source,
                destination,
                amount: 1,
            },
        ],
        json!([
            {"type":"register_account","account":alice.to_string()}, {"type":"register_account","account":bob.to_string()},
            {"type":"register_payment_address","address":source.to_string(),"account":alice.to_string()},
            {"type":"register_payment_address","address":destination.to_string(),"account":bob.to_string()},
            {"type":"issue","recipient":alice.to_string(),"amount":M0_ACCOUNT_ASSETS + 1},
            {"type":"transfer","source":source.to_string(),"destination":destination.to_string(),"amount":1}
        ]),
        network_id,
    );
    cli(&[
        "submit",
        &address_strings[0],
        request.to_str().unwrap(),
        &public_key,
        &certs[0],
    ]);
    wait("four native processes durably pay", || {
        bases.iter().all(|base| {
            snapshot(base, &mut worker)
                .state
                .task_succeeded(task.clone())
                == Some(true)
        })
    });
    for base in &bases {
        let state = snapshot(base, &mut worker).state;
        assert_eq!(state.balance(alice), M0_ACCOUNT_ASSETS);
        assert_eq!(state.balance(bob), 1);
        assert_eq!(state.payment_address_account(source), Some(alice));
        assert_eq!(state.payment_address_account(destination), Some(bob));
    }
    equal_shared(&bases, &mut worker);
    wallet::check(&mut worker, &root, &distro, &bases, &wallet_config);
    // Windows failure: two Linux members are necessary to complete 3/4 quorum.
    drop(windows[0].take());
    let issue = |id| {
        support::write_transaction_request_in_network(
            &request,
            id,
            &authorizer,
            vec![Operation::Issue {
                account: bob,
                count: 1,
            }],
            json!([{"type":"issue","recipient":bob.to_string(),"amount":1}]),
            network_id,
        )
    };
    let outage_task = issue(7102);
    cli(&[
        "submit",
        &address_strings[2],
        request.to_str().unwrap(),
        &public_key,
        &certs[2],
    ]);
    wait("Windows member offline; mixed quorum commits", || {
        bases[1..].iter().all(|base| {
            snapshot(base, &mut worker)
                .state
                .task_succeeded(outage_task.clone())
                == Some(true)
        })
    });
    equal_shared(&bases[1..], &mut worker);
    windows[0] = Some(start_node(executable, addresses[0], &bases[0], 1));
    assert_eq!(windows[0].as_ref().unwrap().certificate, certs[0]);
    cli(&[
        "submit",
        &address_strings[0],
        request.to_str().unwrap(),
        &public_key,
        &certs[0],
    ]);
    wait("restarted Windows member catches up", || {
        snapshot(&bases[0], &mut worker)
            .state
            .task_succeeded(outage_task.clone())
            == Some(true)
    });
    equal_shared(&bases, &mut worker);
    // Lose Linux member 3's signer history; recover into a fresh native Linux base.
    worker.stop(3);
    cli(&[
        "recovery-checkpoint",
        &address_strings[1],
        bases[1].to_str().unwrap(),
        &certs[1],
    ]);
    wait("healthy mixed quorum certifies recovery", || {
        [0, 1, 3].iter().all(|i| {
            snapshot(&bases[*i], &mut worker)
                .recovery_checkpoint_proof
                .is_some()
        })
    });
    let recovered = format!("{linux_root}/recovered-3/second");
    for suffix in [
        ".validator.keys.json",
        ".validator.json",
        ".bootstrap.json",
        ".transport",
    ] {
        worker.request(json!({"action":"copy","source":format!("{}{suffix}",linux_bases[0]),"destination":format!("{recovered}{suffix}")}));
    }
    worker.cli(&[
        "recovery-install",
        &address_strings[1],
        &recovered,
        &linux_bases[1],
        &certs[1],
    ]);
    bases[2] = PathBuf::from(&recovered);
    assert!(!snapshot(&bases[2], &mut worker).validator_safety_ready);
    equal_shared(&bases, &mut worker);
    let rotation = format!("{linux_root}/recovered-3/rotation.request");
    worker.cli(&["validator-rotate", &recovered, "identity", &rotation]);
    let local_rotation = root.join("rotation.request");
    worker.request(
        json!({"action":"copy","source":rotation,"destination":wsl_path(&distro,&local_rotation)}),
    );
    let plan = root.join("transition.json");
    let transition = root.join("transition.source");
    fs::write(
        &plan,
        serde_json::to_vec(&json!({"rotation_request_files":["rotation.request"]})).unwrap(),
    )
    .unwrap();
    cli(&[
        "validator-transition-build",
        bases[1].to_str().unwrap(),
        plan.to_str().unwrap(),
        transition.to_str().unwrap(),
    ]);
    cli(&[
        "validator-transition-submit",
        &address_strings[1],
        bases[1].to_str().unwrap(),
        transition.to_str().unwrap(),
        &certs[1],
    ]);
    wait(
        "healthy members certify V2 while recovered member stays offline",
        || {
            [0, 1, 3]
                .iter()
                .all(|i| snapshot(&bases[*i], &mut worker).validator_set.version() == 2)
        },
    );
    for node in &mut windows {
        drop(node.take());
    }
    worker.stop(4);
    for i in 0..2 {
        windows[i] = Some(start_node(
            executable,
            addresses[i],
            &bases[i],
            i as u64 + 1,
        ));
    }
    worker.start(4, &address_strings[3], bases[3].to_str().unwrap());
    worker.start(3, &address_strings[2], &recovered);
    wait(
        "late recovered Linux member pulls V2 durable proof after every provider restarts",
        || snapshot(&bases[2], &mut worker).validator_set.version() == 2,
    );
    assert!(!snapshot(&bases[2], &mut worker).validator_safety_ready);
    worker.stop(3);
    worker.start(3, &address_strings[2], &recovered);
    assert!(!snapshot(&bases[2], &mut worker).validator_safety_ready);
    cli(&[
        "recovery-checkpoint",
        &address_strings[1],
        bases[1].to_str().unwrap(),
        &certs[1],
    ]);
    wait("Linux safety ready with V2 fence after restart", || {
        let state = snapshot(&bases[2], &mut worker);
        state.validator_safety_ready && state.minimum_signing_validator_set_version == 2
    });
    worker.stop(3);
    let restarted = worker.start(3, &address_strings[2], &recovered);
    assert_eq!(restarted[3], linux_three[3]);
    assert_eq!(restarted[5], certs[2]);
    worker.stop(4);
    let recovered_task = issue(7103);
    let linux_request = format!("{linux_root}/last-business.json");
    worker.request(
        json!({"action":"copy","source":wsl_path(&distro,&request),"destination":linux_request}),
    );
    worker.cli(&[
        "submit",
        &address_strings[2],
        &linux_request,
        &public_key,
        &certs[2],
    ]);
    wait(
        "recovered Linux member necessary for new 3/4 business quorum",
        || {
            bases[..3].iter().all(|base| {
                snapshot(base, &mut worker)
                    .state
                    .task_succeeded(recovered_task.clone())
                    == Some(true)
            })
        },
    );
    for base in &bases[..3] {
        let state = snapshot(base, &mut worker);
        assert_eq!(state.state.balance(bob), 3);
        assert_eq!(state.state.balance(alice), M0_ACCOUNT_ASSETS);
        if base == &bases[2] {
            assert_eq!(state.minimum_signing_validator_set_version, 2);
        }
    }
    equal_shared(&bases[..3], &mut worker);
    if soak_seconds > 0 {
        soak::run(
            Duration::from_secs(soak_seconds),
            &mut worker,
            &mut windows,
            &bases,
            &addresses,
            &certs,
            &request,
            &authorizer,
            &public_key,
            source,
            destination,
            network_id,
        );
    }
    drop(windows);
    worker.request(json!({"action":"finish"}));
    drop(worker);
    println!("MIXED-PASS full canonical shared payload identical; all owned children stopped");
    fs::remove_dir_all(&root).unwrap();
}
