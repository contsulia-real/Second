use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::json;

use crate::support::{account, key, payment, temp_base, write_transaction_request};
use second::{Operation, PaymentAddressStatus, StateStore};

pub(crate) struct RunningNode {
    pub(crate) child: Child,
    pub(crate) address: String,
    pub(crate) certificate: String,
    pub(crate) node_id: String,
}

impl Drop for RunningNode {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if std::thread::panicking()
            && let Some(stderr) = self.child.stderr.as_mut()
        {
            let mut diagnostic = String::new();
            let _ = stderr.read_to_string(&mut diagnostic);
            if !diagnostic.is_empty() {
                eprintln!("node {} stderr: {diagnostic}", self.node_id);
            }
        }
    }
}

#[test]
fn init_network_rejects_validator_set_larger_than_complete_bootstrap_capacity() {
    let executable = env!("CARGO_BIN_EXE_second");
    let root = temp_base("cli-network-init-too-many");
    let config_path = root.with_extension("network.json");
    let output_dir = root.with_extension("network");
    let authorizer = key(9);

    let validators = (1_u64..=second::MAX_DEPLOYED_VALIDATORS as u64 + 1)
        .map(|validator_id| {
            json!({
                "validator_id": validator_id,
                "listen_address": format!("127.0.0.1:{}", 30000 + validator_id),
                "keyring_file": format!("missing-{validator_id}.keys.json")
            })
        })
        .collect::<Vec<_>>();
    let config = json!({
        "validator_set_version": 1,
        "first_currency_address": 1,
        "reserve_count": 0,
        "accounts": [account(502).to_string()],
        "authorizer_public_keys_base64": [
            STANDARD.encode(authorizer.verifying_key().to_bytes())
        ],
        "bft_timeouts_ms": {
            "proposal": 1000,
            "prevote": 1000,
            "precommit": 1000
        },
        "validators": validators
    });
    fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();

    let output = Command::new(executable)
        .args([
            "init-network",
            config_path.to_str().unwrap(),
            output_dir.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains(&format!(
            "at most {} genesis validators",
            second::MAX_DEPLOYED_VALIDATORS
        )),
        "{stderr}"
    );
    assert!(!output_dir.exists());
    assert!(!PathBuf::from(format!("{}.new", output_dir.display())).exists());

    fs::remove_file(config_path).unwrap();
}

#[test]
fn real_cli_four_validator_network_registers_accounts_and_completes_payment_after_restart() {
    let executable = env!("CARGO_BIN_EXE_second");
    let root = temp_base("cli-network-init");
    let config_path = root.with_extension("network.json");
    let output_dir = root.with_extension("network");
    let request_path = root.with_extension("request.json");
    let authorizer = key(9);
    let recipient = account(501);
    let receiver = account(502);
    let source = payment(501);
    let destination = payment(502);

    let mut reservations = Vec::new();
    let mut addresses = Vec::new();
    for _ in 0..4 {
        let socket = crate::support::ports::reserve_node_listen_socket();
        addresses.push(socket.local_addr().unwrap());
        reservations.push(socket);
    }

    let keyring_paths = (1_u64..=4)
        .map(|validator_id| root.with_extension(format!("validator-{validator_id}.keys.json")))
        .collect::<Vec<_>>();
    for (index, path) in keyring_paths.iter().enumerate() {
        let output = Command::new(executable)
            .args([
                "validator-keygen",
                &(index as u64 + 1).to_string(),
                path.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "validator-keygen failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.contains(&format!("VALIDATOR-CREDENTIAL validator={}", index + 1)));
    }

    let validators = addresses
        .iter()
        .enumerate()
        .map(|(index, address)| {
            json!({
                "validator_id": index + 1,
                "listen_address": address.to_string(),
                "keyring_file": keyring_paths[index].file_name().unwrap().to_str().unwrap()
            })
        })
        .collect::<Vec<_>>();
    let config = json!({
        "validator_set_version": 1,
        "first_currency_address": 1,
        "reserve_count": 2,
        "accounts": [],
        "authorizer_public_keys_base64": [
            STANDARD.encode(authorizer.verifying_key().to_bytes())
        ],
        "bft_timeouts_ms": {
            "proposal": 1000,
            "prevote": 1000,
            "precommit": 1000
        },
        "validators": validators
    });
    fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();

    let initialized = Command::new(executable)
        .args([
            "init-network",
            config_path.to_str().unwrap(),
            output_dir.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        initialized.status.success(),
        "init-network failed: {}",
        String::from_utf8_lossy(&initialized.stderr)
    );
    let initialized_stdout = String::from_utf8(initialized.stdout).unwrap();
    assert_eq!(
        initialized_stdout
            .lines()
            .filter(|line| line.starts_with("INITIALIZED validator="))
            .count(),
        4
    );

    let overwrite = Command::new(executable)
        .args([
            "init-network",
            config_path.to_str().unwrap(),
            output_dir.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!overwrite.status.success());
    assert!(
        String::from_utf8_lossy(&overwrite.stderr).contains("already exists"),
        "unexpected overwrite rejection: {}",
        String::from_utf8_lossy(&overwrite.stderr)
    );

    drop(reservations);

    let snapshot_bases = (1_u64..=4)
        .map(|validator_id| {
            output_dir
                .join(format!("validator-{validator_id}"))
                .join("second")
        })
        .collect::<Vec<_>>();
    for base in &snapshot_bases {
        let status = snapshot_status(executable, base);
        assert!(status.contains("supply=2"), "{status}");
        assert!(status.contains("reserve=2"), "{status}");
        assert!(status.contains("validator_set=1"), "{status}");
        assert!(status.contains("validators=4"), "{status}");
        assert!(status.contains("quorum=3"), "{status}");
    }

    let mut nodes = addresses
        .iter()
        .zip(snapshot_bases.iter())
        .enumerate()
        .map(|(index, (address, base))| start_node(executable, *address, base, index as u64 + 1))
        .collect::<Vec<_>>();

    thread::sleep(Duration::from_secs(3));

    let untrusted = key(88);
    let intruder = account(777);
    let intruder_task_id = write_transaction_request(
        &request_path,
        9100,
        &untrusted,
        vec![Operation::RegisterAccount { account: intruder }],
        json!([{ "type": "register_account", "account": intruder.to_string() }]),
    );
    let rejected = Command::new(executable)
        .args([
            "submit",
            nodes[1].address.as_str(),
            request_path.to_str().unwrap(),
            &STANDARD.encode(untrusted.verifying_key().to_bytes()),
            nodes[1].certificate.as_str(),
        ])
        .output()
        .unwrap();
    assert!(
        !rejected.status.success(),
        "untrusted issuer registered an account"
    );
    for base in &snapshot_bases {
        let state = StateStore::new(base).load().unwrap().unwrap().state;
        assert!(!state.has_account(intruder));
        assert!(
            state
                .bound_request_digest(intruder_task_id.clone())
                .is_none()
        );
    }
    let wallet_config: serde_json::Value =
        serde_json::from_slice(&fs::read(output_dir.join("wallet-network.json")).unwrap()).unwrap();
    let network_id: [u8; 32] = STANDARD
        .decode(wallet_config["network_id_base64"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let submitted_task_id = crate::support::write_transaction_request_in_network(
        &request_path,
        9101,
        &authorizer,
        vec![
            Operation::RegisterAccount { account: recipient },
            Operation::RegisterAccount { account: receiver },
            Operation::RegisterPaymentAddress {
                address: source,
                account: recipient,
            },
            Operation::RegisterPaymentAddress {
                address: destination,
                account: receiver,
            },
            Operation::Issue {
                account: recipient,
                count: 1,
            },
            Operation::Transfer {
                source,
                destination,
                amount: 1,
            },
            Operation::RetirePaymentAddress { address: source },
            Operation::FinalizePaymentAddressRetirement { address: source },
        ],
        json!([
            { "type": "register_account", "account": recipient.to_string() },
            { "type": "register_account", "account": receiver.to_string() },
            { "type": "register_payment_address", "address": source.to_string(), "account": recipient.to_string() },
            { "type": "register_payment_address", "address": destination.to_string(), "account": receiver.to_string() },
            { "type": "issue", "recipient": recipient.to_string(), "amount": 1 },
            { "type": "transfer", "source": source.to_string(), "destination": destination.to_string(), "amount": 1 },
            { "type": "retire_payment_address", "address": source.to_string() },
            { "type": "finalize_payment_address_retirement", "address": source.to_string() },
        ]),
        network_id,
    );
    let submit = Command::new(executable)
        .args([
            "submit",
            nodes[1].address.as_str(),
            request_path.to_str().unwrap(),
            &STANDARD.encode(authorizer.verifying_key().to_bytes()),
            nodes[1].certificate.as_str(),
        ])
        .output()
        .unwrap();
    assert!(
        submit.status.success(),
        "four-validator submission failed: {}",
        String::from_utf8_lossy(&submit.stderr)
    );
    let submit_stdout = String::from_utf8(submit.stdout).unwrap();
    assert!(
        submit_stdout.contains(&format!("ACCEPTED task={submitted_task_id} state=")),
        "{submit_stdout}"
    );

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let committed = snapshot_bases.iter().all(|base| {
            let status = snapshot_status(executable, base);
            status.contains("supply=3") && status.contains("reserve=2")
        });
        if committed {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "four-validator network did not durably commit on every node before deadline"
        );
        thread::sleep(Duration::from_millis(100));
    }

    for base in &snapshot_bases {
        let state = StateStore::new(base).load().unwrap().unwrap().state;
        assert!(state.has_account(recipient) && state.has_account(receiver));
        assert_eq!(state.balance(recipient), 0);
        assert_eq!(state.balance(receiver), 1);
        assert_eq!(state.payment_address_account(source), Some(recipient));
        assert_eq!(state.payment_address_account(destination), Some(receiver));
        assert_eq!(
            state.payment_address_status(source),
            Some(PaymentAddressStatus::Retired)
        );
        assert_eq!(state.payment_execution_count(), 0);
    }

    let public = Command::new(executable)
        .args([
            "sync-public",
            nodes[3].address.as_str(),
            nodes[3].certificate.as_str(),
        ])
        .output()
        .unwrap();
    assert!(
        public.status.success(),
        "public sync after consensus failed: {}",
        String::from_utf8_lossy(&public.stderr)
    );
    let public_stdout = String::from_utf8(public.stdout).unwrap();
    assert!(public_stdout.contains("supply=3"), "{public_stdout}");
    assert!(public_stdout.contains("reserve=2"), "{public_stdout}");

    let old_node_id = nodes[3].node_id.clone();
    let old_certificate = nodes[3].certificate.clone();
    drop(nodes.pop());

    let restarted = start_node(executable, addresses[3], &snapshot_bases[3], 4);
    assert_eq!(restarted.node_id, old_node_id);
    assert_eq!(restarted.certificate, old_certificate);
    let retry = Command::new(executable)
        .args([
            "submit",
            restarted.address.as_str(),
            request_path.to_str().unwrap(),
            &STANDARD.encode(authorizer.verifying_key().to_bytes()),
            restarted.certificate.as_str(),
        ])
        .output()
        .unwrap();
    assert!(
        retry.status.success(),
        "{}",
        String::from_utf8_lossy(&retry.stderr)
    );
    assert!(String::from_utf8_lossy(&retry.stdout).contains("state=succeeded"));
    let state = StateStore::new(&snapshot_bases[3])
        .load()
        .unwrap()
        .unwrap()
        .state;
    assert!(state.has_account(recipient) && state.has_account(receiver));
    assert_eq!(state.balance(receiver), 1);
    assert_eq!(
        state.payment_address_status(source),
        Some(PaymentAddressStatus::Retired)
    );
    assert!(
        snapshot_status(executable, &snapshot_bases[3]).contains("supply=3"),
        "restarted validator lost durable committed state"
    );
    let ping = Command::new(executable)
        .args([
            "ping",
            restarted.address.as_str(),
            "1234",
            restarted.certificate.as_str(),
        ])
        .output()
        .unwrap();
    assert!(
        ping.status.success(),
        "restarted validator was not reachable: {}",
        String::from_utf8_lossy(&ping.stderr)
    );

    drop(restarted);
    drop(nodes);

    #[cfg(any(windows, target_os = "linux"))]
    {
        #[cfg(windows)]
        let mut supervisor = {
            let mut command = Command::new("pwsh");
            command.args([
                "-NoProfile",
                "-File",
                "examples/run-network.ps1",
                "-SecondExe",
                executable,
                "-ConfigFile",
                config_path.to_str().unwrap(),
                "-DeploymentDirectory",
                output_dir.to_str().unwrap(),
                "-RunSeconds",
                "1",
            ]);
            command
        };
        #[cfg(target_os = "linux")]
        let mut supervisor = {
            let mut command = Command::new("python3");
            command.args([
                "examples/run-network.py",
                "--second-exe",
                executable,
                "--config-file",
                config_path.to_str().unwrap(),
                "--deployment-directory",
                output_dir.to_str().unwrap(),
                "--run-seconds",
                "1",
            ]);
            command
        };
        let supervised = supervisor.output().unwrap();
        assert!(
            supervised.status.success(),
            "{}",
            String::from_utf8_lossy(&supervised.stderr)
        );
        let stdout = String::from_utf8(supervised.stdout).unwrap();
        assert_eq!(
            stdout
                .lines()
                .filter(|line| line.starts_with("LISTENING "))
                .count(),
            4
        );
        assert!(stdout.contains("ENDPOINTS "));
        let inventory = stdout
            .lines()
            .find_map(|line| line.strip_prefix("ENDPOINTS "))
            .unwrap();
        let endpoints: serde_json::Value =
            serde_json::from_slice(&fs::read(inventory).unwrap()).unwrap();
        assert_eq!(endpoints.as_array().unwrap().len(), 4);
        assert_eq!(endpoints[3]["certificate_base64"], old_certificate);
        #[cfg(target_os = "linux")]
        {
            // Real SIGTERM must reap this invocation's child and release its lock.
            let mut signal_run = Command::new("python3")
                .args([
                    "examples/run-network.py",
                    "--second-exe",
                    executable,
                    "--config-file",
                    config_path.to_str().unwrap(),
                    "--deployment-directory",
                    output_dir.to_str().unwrap(),
                    "--validator-ids",
                    "4",
                ])
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            let mut output = BufReader::new(signal_run.stdout.take().unwrap());
            let mut line = String::new();
            output.read_line(&mut line).unwrap();
            assert!(line.starts_with("LISTENING "), "{line}");
            line.clear();
            output.read_line(&mut line).unwrap();
            assert!(line.starts_with("ENDPOINTS "), "{line}");
            assert!(
                Command::new("kill")
                    .args(["-TERM", &signal_run.id().to_string()])
                    .status()
                    .unwrap()
                    .success()
            );
            assert!(signal_run.wait().unwrap().success());
        }
        let after_supervisor = start_node(executable, addresses[3], &snapshot_bases[3], 4);
        assert_eq!(after_supervisor.certificate, old_certificate);
        assert_eq!(after_supervisor.node_id, old_node_id);
        drop(after_supervisor);
    }

    crate::cli_node_reentry::verify_reentry(
        executable,
        &addresses,
        &snapshot_bases,
        &output_dir,
        receiver,
        &authorizer,
    );

    fs::remove_dir_all(&output_dir).unwrap();
    fs::remove_file(&config_path).unwrap();
    fs::remove_file(&request_path).unwrap();
    for path in keyring_paths {
        fs::remove_file(path).unwrap();
    }
}

#[test]
fn real_cli_provisions_and_loads_complete_bootstrap_beyond_public_page_limit() {
    let executable = env!("CARGO_BIN_EXE_second");
    let root = temp_base("cli-large-network-init");
    let config_path = root.with_extension("network.json");
    let output_dir = root.with_extension("network");
    let mut sockets = Vec::new();
    let mut keys = Vec::new();
    let mut validators = Vec::new();
    for id in 1_u64..=34 {
        let socket = crate::support::ports::reserve_node_listen_socket();
        let path = root.with_extension(format!("{id}.keys.json"));
        let output = Command::new(executable)
            .args(["validator-keygen", &id.to_string(), path.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        validators.push(json!({ "validator_id": id, "listen_address": socket.local_addr().unwrap().to_string(), "keyring_file": path.to_str().unwrap() }));
        sockets.push(socket);
        keys.push(path);
    }
    fs::write(&config_path, serde_json::to_vec(&json!({
        "validator_set_version": 1, "first_currency_address": 1, "reserve_count": 0,
        "accounts": [], "authorizer_public_keys_base64": [STANDARD.encode(key(9).verifying_key().to_bytes())],
        "bft_timeouts_ms": { "proposal": 1000, "prevote": 1000, "precommit": 1000 },
        "validators": validators
    })).unwrap()).unwrap();
    let output = Command::new(executable)
        .args([
            "init-network",
            config_path.to_str().unwrap(),
            output_dir.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for id in 1..=34 {
        let base = output_dir.join(format!("validator-{id}")).join("second");
        assert_eq!(
            second::StateStore::new(&base)
                .load()
                .unwrap()
                .unwrap()
                .validator_set
                .len(),
            34
        );
        let bootstrap: Vec<serde_json::Value> = serde_json::from_slice(
            &fs::read(PathBuf::from(format!("{}.bootstrap.json", base.display()))).unwrap(),
        )
        .unwrap();
        assert_eq!(bootstrap.len(), 33);
        assert_eq!(
            bootstrap
                .iter()
                .map(|entry| entry["node_id"].as_str().unwrap())
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            33
        );
    }
    let address = sockets.remove(0).local_addr().unwrap();
    let base = output_dir.join("validator-1/second");
    // Releasing the first socket lets the real daemon parse and load its 33-record bootstrap.
    let node = start_node(executable, address, &base, 1);
    drop(node);
    drop(sockets);
    fs::remove_dir_all(output_dir).unwrap();
    fs::remove_file(config_path).unwrap();
    for path in keys {
        fs::remove_file(path).unwrap();
    }
}

pub(crate) fn start_node(
    executable: &str,
    address: SocketAddr,
    snapshot_base: &Path,
    validator_id: u64,
) -> RunningNode {
    let mut child = Command::new(executable)
        .args([
            "node",
            &address.to_string(),
            snapshot_base.to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    if line.is_empty() {
        let output = child.wait_with_output().unwrap();
        panic!(
            "validator {validator_id} exited before startup: status={} stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let fields = line.split_whitespace().collect::<Vec<_>>();
    assert_eq!(
        fields.len(),
        8,
        "unexpected validator node startup line: {line}"
    );
    assert_eq!(fields[0], "LISTENING");
    assert_eq!(fields[1], address.to_string());
    assert_eq!(fields[2], "NODE");
    assert_eq!(fields[4], "CERT");
    assert_eq!(fields[6], "VALIDATOR");
    assert_eq!(fields[7], validator_id.to_string());

    RunningNode {
        child,
        address: fields[1].to_owned(),
        node_id: fields[3].to_owned(),
        certificate: fields[5].to_owned(),
    }
}

fn snapshot_status(executable: &str, snapshot_base: &Path) -> String {
    let output = Command::new(executable)
        .args(["snapshot-status", snapshot_base.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "snapshot-status failed for {}: {}",
        snapshot_base.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}
