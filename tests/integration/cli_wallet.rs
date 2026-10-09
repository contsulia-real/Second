//! Acceptance through the real wallet CLI, rather than a test-only account signer.
use std::fs;
use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use second::{AccountAddress, Operation, PaymentAddress, StateStore, TaskId};
use serde_json::{Value, json};

use crate::cli_network_init::{RunningNode, start_node};
use crate::support::{self, key, temp_base};

struct TestDirectory(PathBuf);

impl Drop for TestDirectory {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.0)
            && self.0.exists()
        {
            eprintln!(
                "failed to clean wallet acceptance fixture {}: {error}",
                self.0.display()
            );
        }
    }
}

fn invoke(executable: &str, args: &[&str]) -> Output {
    Command::new(executable).args(args).output().unwrap()
}

fn checked(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn wallet(executable: &str, name: &str, dir: &Path, pass: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(executable);
    cmd.arg("wallet")
        .arg(name)
        .arg(dir)
        .args(args)
        .arg("--password-file")
        .arg(pass);
    cmd.output().unwrap()
}

// The exact intent is saved before network submission; retry it on a lost ACK.
fn wallet_submit(exe: &str, command: &str, dir: &Path, pass: &Path, args: &[&str]) -> String {
    let first = wallet(exe, command, dir, pass, args);
    let output = String::from_utf8(first.stdout).unwrap();
    let id = field(&output, "task=").to_owned();
    if first.status.success() {
        return output;
    }
    let failure = String::from_utf8_lossy(&first.stderr);
    assert!(
        failure.contains("remains saved"),
        "wallet intent was not durably saved: {failure}"
    );
    let mut last = failure.to_string();
    for _ in 0..4 {
        let retry = wallet(exe, "retry", dir, pass, &[&id]);
        if retry.status.success() {
            return format!("{output}\\n{}", String::from_utf8(retry.stdout).unwrap());
        }
        last = String::from_utf8_lossy(&retry.stderr).to_string();
    }
    panic!("durable wallet retry failed: {last}");
}

fn field<'a>(text: &'a str, name: &str) -> &'a str {
    text.split_whitespace()
        .find_map(|part| part.strip_prefix(name))
        .unwrap_or_else(|| panic!("missing {name} in {text}"))
}

fn until(label: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        if ready() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "wallet acceptance timed out: {label}"
        );
        thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn two_real_wallets_transfer_without_authorizer_and_reject_tampering() {
    let exe = env!("CARGO_BIN_EXE_second");
    let root = temp_base("real-wallet-acceptance");
    fs::create_dir(&root).unwrap();
    let _cleanup = TestDirectory(root.clone());
    let network_config = root.join("network.json");
    let deployment = root.join("network");
    let manager = key(9);

    let mut reserved = Vec::new();
    let mut listeners = Vec::new();
    for id in 1_u64..=4 {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let keyring = root.join(format!("validator-{id}.keys.json"));
        checked(invoke(
            exe,
            &[
                "validator-keygen",
                &id.to_string(),
                keyring.to_str().unwrap(),
            ],
        ));
        listeners.push(socket.local_addr().unwrap());
        reserved.push(socket);
    }
    let validators = listeners
        .iter()
        .enumerate()
        .map(|(index, address)| {
            json!({
                "validator_id": index + 1,
                "listen_address": address.to_string(),
                "keyring_file": format!("validator-{}.keys.json", index + 1),
            })
        })
        .collect::<Vec<_>>();
    fs::write(
        &network_config,
        serde_json::to_vec(&json!({
            "validator_set_version": 1,
            "first_currency_address": 1,
            "reserve_count": 0,
            "accounts": [],
            "authorizer_public_keys_base64": [STANDARD.encode(manager.verifying_key().to_bytes())],
            "bft_timeouts_ms": {"proposal": 1000, "prevote": 1000, "precommit": 1000},
            "validators": validators,
        }))
        .unwrap(),
    )
    .unwrap();
    checked(invoke(
        exe,
        &[
            "init-network",
            network_config.to_str().unwrap(),
            deployment.to_str().unwrap(),
        ],
    ));
    let wallet_config = deployment.join("wallet-network.json");
    let config: Value = serde_json::from_slice(&fs::read(&wallet_config).unwrap()).unwrap();
    let network_id: [u8; 32] = STANDARD
        .decode(config["network_id_base64"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();

    drop(reserved);
    let bases = (1..=4)
        .map(|id| deployment.join(format!("validator-{id}")).join("second"))
        .collect::<Vec<_>>();
    let nodes: Vec<RunningNode> = listeners
        .iter()
        .zip(&bases)
        .enumerate()
        .map(|(index, (address, base))| start_node(exe, *address, base, index as u64 + 1))
        .collect();
    thread::sleep(Duration::from_secs(2));

    let pass = root.join("password");
    fs::write(&pass, b"temporary-test-only-strong-password\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&pass, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let alice_dir = root.join("alice");
    let bob_dir = root.join("bob");
    for directory in [&alice_dir, &bob_dir] {
        checked(invoke(
            exe,
            &[
                "wallet",
                "init",
                directory.to_str().unwrap(),
                bases[0].to_str().unwrap(),
                wallet_config.to_str().unwrap(),
                "--password-file",
                pass.to_str().unwrap(),
            ],
        ));
    }
    let alice_address = checked(wallet(exe, "address", &alice_dir, &pass, &[]));
    let bob_address = checked(wallet(exe, "address", &bob_dir, &pass, &[]));
    let alice = AccountAddress::parse(field(&alice_address, "account=")).unwrap();
    let bob = AccountAddress::parse(field(&bob_address, "account=")).unwrap();
    let bob_payment = PaymentAddress::parse(field(&bob_address, "payment=")).unwrap();
    let alice_payment = PaymentAddress::parse(field(&alice_address, "payment=")).unwrap();
    assert_ne!(alice, bob);

    let register_alice = wallet_submit(exe, "register", &alice_dir, &pass, &[]);
    let register_bob = wallet_submit(exe, "register", &bob_dir, &pass, &[]);
    assert!(
        !register_alice.contains("awaiting_authorizer"),
        "{register_alice}"
    );
    assert!(
        !register_bob.contains("awaiting_authorizer"),
        "{register_bob}"
    );
    until("accounts registered on all validators", || {
        bases.iter().all(|base| {
            let state = StateStore::new(base).load().unwrap().unwrap().state;
            state.payment_address_account(alice_payment) == Some(alice)
                && state.payment_address_account(bob_payment) == Some(bob)
        })
    });

    let request = root.join("issue.json");
    let issue_id = support::write_transaction_request_in_network(
        &request,
        900001,
        &manager,
        vec![Operation::Issue {
            account: alice,
            count: 2,
        }],
        json!([{"type":"issue", "recipient":alice.to_string(), "amount":2}]),
        network_id,
    );
    checked(invoke(
        exe,
        &[
            "submit",
            &nodes[0].address,
            request.to_str().unwrap(),
            &STANDARD.encode(manager.verifying_key().to_bytes()),
            &nodes[0].certificate,
        ],
    ));
    until("manager issuance", || {
        bases.iter().all(|base| {
            let state = StateStore::new(base).load().unwrap().unwrap().state;
            state.task_succeeded(issue_id.clone()) == Some(true) && state.balance(alice) == 2
        })
    });

    // Neither wallet contains an Authorizer private key.
    let send = checked(wallet(
        exe,
        "send",
        &alice_dir,
        &pass,
        &[&bob_payment.to_string(), "1", "--yes"],
    ));
    assert!(!send.contains("awaiting_authorizer"), "{send}");
    let task_text = field(&send, "task=");
    let transfer_id = TaskId::parse(task_text).unwrap();
    until("wallet-funded payment finalized", || {
        bases.iter().all(|base| {
            let state = StateStore::new(base).load().unwrap().unwrap().state;
            state.task_succeeded(transfer_id.clone()) == Some(true)
                && state.balance(alice) == 1
                && state.balance(bob) == 1
        })
    });

    // The original request must remain identical and replay must not double debit.
    let status = checked(wallet(exe, "status", &alice_dir, &pass, &[task_text]));
    assert!(status.contains("state=succeeded"), "{status}");
    let retry = checked(wallet(exe, "retry", &alice_dir, &pass, &[task_text]));
    assert!(retry.contains("state=succeeded"), "{retry}");
    let bob_balance = checked(wallet(exe, "balance", &bob_dir, &pass, &[]));
    assert!(bob_balance.contains("\"balance\": 1"), "{bob_balance}");

    // Export only the intended request and signature, then mutate the payload.
    // A wrong amount and a signature falsely attributed to Bob must both fail
    // before the existing TaskId can be rebound.
    let export = root.join("exported-intent.json");
    checked(wallet(
        exe,
        "export-request",
        &alice_dir,
        &pass,
        &[task_text, export.to_str().unwrap()],
    ));
    let exported: Value = serde_json::from_slice(&fs::read(export).unwrap()).unwrap();
    let mut tampered = exported["unsigned_transaction"].clone();
    tampered["operations"][0]["amount"] = json!(2);
    tampered["account_signatures"] = json!([{
        "account": exported["account"], "signature": exported["account_signature_base64"],
    }]);
    let tampered_path = root.join("tampered.json");
    fs::write(&tampered_path, serde_json::to_vec(&tampered).unwrap()).unwrap();
    let rejected = invoke(
        exe,
        &[
            "submit",
            &nodes[0].address,
            tampered_path.to_str().unwrap(),
            &STANDARD.encode(manager.verifying_key().to_bytes()),
            &nodes[0].certificate,
        ],
    );
    assert!(
        !rejected.status.success(),
        "tampered wallet intent was accepted"
    );
    tampered["account_signatures"][0]["account"] = json!(bob.to_string());
    fs::write(&tampered_path, serde_json::to_vec(&tampered).unwrap()).unwrap();
    let wrong_owner = invoke(
        exe,
        &[
            "submit",
            &nodes[0].address,
            tampered_path.to_str().unwrap(),
            &STANDARD.encode(manager.verifying_key().to_bytes()),
            &nodes[0].certificate,
        ],
    );
    assert!(
        !wrong_owner.status.success(),
        "wrong account owner was accepted"
    );
    assert!(bases.iter().all(|base| {
        let state = StateStore::new(base).load().unwrap().unwrap().state;
        state.balance(alice) == 1
            && state.balance(bob) == 1
            && state.task_succeeded(transfer_id.clone()) == Some(true)
    }));

    drop(nodes);
    // No fixture snapshots, keyrings, plaintext password, or wallet vaults survive.
}
