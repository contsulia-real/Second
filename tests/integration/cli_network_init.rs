use std::fs;
use std::io::{BufRead, BufReader};
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::json;

use crate::support::{account, key, temp_base, write_issue_transaction_request};

struct RunningNode {
    child: Child,
    address: String,
    certificate: String,
    node_id: String,
}

impl Drop for RunningNode {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn init_network_rejects_validator_set_larger_than_complete_bootstrap_capacity() {
    let executable = env!("CARGO_BIN_EXE_second");
    let root = temp_base("cli-network-init-too-many");
    let config_path = root.with_extension("network.json");
    let output_dir = root.with_extension("network");
    let authorizer = key(9);

    let validators = (1_u64..=34)
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
            "proposal": 250,
            "prevote": 250,
            "precommit": 250
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
    assert!(stderr.contains("at most 33 genesis validators"), "{stderr}");
    assert!(!output_dir.exists());
    assert!(!PathBuf::from(format!("{}.new", output_dir.display())).exists());

    fs::remove_file(config_path).unwrap();
}

#[test]
fn real_cli_initializes_four_validator_network_and_commits_after_restart() {
    let executable = env!("CARGO_BIN_EXE_second");
    let root = temp_base("cli-network-init");
    let config_path = root.with_extension("network.json");
    let output_dir = root.with_extension("network");
    let request_path = root.with_extension("request.json");
    let authorizer = key(9);
    let recipient = account(501);

    let mut reservations = Vec::new();
    let mut addresses = Vec::new();
    for _ in 0..4 {
        let socket = UdpSocket::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).unwrap();
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
        "accounts": [recipient.to_string()],
        "authorizer_public_keys_base64": [
            STANDARD.encode(authorizer.verifying_key().to_bytes())
        ],
        "bft_timeouts_ms": {
            "proposal": 250,
            "prevote": 250,
            "precommit": 250
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

    let submitted_task_id =
        write_issue_transaction_request(&request_path, recipient, 9101, &authorizer, 1);
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

    let deadline = Instant::now() + Duration::from_secs(20);
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

    fs::remove_dir_all(&output_dir).unwrap();
    fs::remove_file(&config_path).unwrap();
    fs::remove_file(&request_path).unwrap();
    for path in keyring_paths {
        fs::remove_file(path).unwrap();
    }
}

fn start_node(
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
