//! Explicit sustained business and process failure acceptance on the same cluster.
use super::*;
use crate::cli_network_init::RunningNode;
use ed25519_dalek::SigningKey;
use second::PaymentAddress;
use std::os::windows::process::CommandExt;

// Keep ownership in the existing fixture rather than build another cluster model.
#[allow(clippy::too_many_arguments)]
pub(super) fn run(
    duration: Duration,
    worker: &mut LinuxWorker,
    windows: &mut [Option<RunningNode>],
    bases: &[PathBuf],
    addresses: &[std::net::SocketAddr],
    certs: &[String],
    request: &Path,
    authorizer: &SigningKey,
    public_key: &str,
    source: PaymentAddress,
    destination: PaymentAddress,
    network_id: [u8; 32],
) {
    let started = Instant::now();
    let addresses = addresses
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let first_request = request.with_file_name("soak-first.json");
    let initial = snapshot(&bases[0], worker).state;
    let alice = initial.payment_address_account(source).unwrap();
    let bob = initial.payment_address_account(destination).unwrap();
    let balances = (initial.balance(alice), initial.balance(bob));
    worker.start(4, &addresses[3], bases[3].to_str().unwrap());
    replay(3, request, worker, &addresses, certs, public_key);
    wait(
        "soak fourth member catches up by exact request replay",
        || snapshot(&bases[3], worker).payload == snapshot(&bases[0], worker).payload,
    );
    let mut offline = None;
    let mut round = 0_u128;
    let mut latencies = Vec::new();
    let mut missed = Vec::new();
    while started.elapsed() < duration || round < 96 {
        if round.is_multiple_of(16) {
            let index = (round / 16 % 4) as usize;
            if index < 2 {
                drop(windows[index].take());
            } else {
                worker.stop(index as u64 + 1);
            }
            offline = Some(index);
        }
        let active = (0..4)
            .filter(|index| Some(*index) != offline)
            .collect::<Vec<_>>();
        let active_bases = active
            .iter()
            .map(|index| bases[*index].clone())
            .collect::<Vec<_>>();
        let (from, to) = if round.is_multiple_of(2) {
            (source, destination)
        } else {
            (destination, source)
        };
        let task = write_transaction_request_in_network(
            request,
            7200 + round,
            authorizer,
            vec![Operation::Transfer {
                source: from,
                destination: to,
                amount: 1,
            }],
            json!([{"type":"transfer", "source":from.to_string(), "destination":to.to_string(), "amount":1}]),
            network_id,
        );
        if round == 0 {
            fs::copy(request, &first_request).unwrap();
        }
        if offline.is_some() {
            let saved = request.with_file_name(format!("soak-missed-{round}.json"));
            fs::copy(request, &saved).unwrap();
            missed.push((saved, task.clone()));
        }
        let submitted = Instant::now();
        let index = active[round as usize % active.len()];
        replay(index, request, worker, &addresses, certs, public_key);
        wait("soak active quorum durably transfers", || {
            active_bases
                .iter()
                .all(|base| snapshot(base, worker).state.task_succeeded(task.clone()) == Some(true))
        });
        latencies.push(submitted.elapsed().as_millis());
        let expected = if round.is_multiple_of(2) {
            (balances.0 - 1, balances.1 + 1)
        } else {
            balances
        };
        for base in &active_bases {
            let current = snapshot(base, worker);
            assert_eq!(
                (current.state.balance(alice), current.state.balance(bob)),
                expected
            );
            assert!(current.validator_safety_ready);
            assert_eq!(current.validator_set.version(), 2);
            if base == &bases[2] {
                assert_eq!(current.minimum_signing_validator_set_version, 2);
            }
        }
        equal_shared(&active_bases, worker);
        if round % 16 == 3 {
            let index = offline.take().unwrap();
            restart(index, worker, windows, bases, &addresses, certs);
            for (saved, task) in missed.drain(..) {
                replay(index, &saved, worker, &addresses, certs, public_key);
                wait("soak exact missed request durably completes", || {
                    snapshot(&bases[index], worker)
                        .state
                        .task_succeeded(task.clone())
                        == Some(true)
                });
                fs::remove_file(saved).unwrap();
            }
            wait(
                "soak restarted member catches up all missed transfers",
                || {
                    let expected = snapshot(&bases[0], worker).payload;
                    bases[1..]
                        .iter()
                        .all(|base| snapshot(base, worker).payload == expected)
                },
            );
        }
        if round == 95 {
            let before = snapshot(&active_bases[0], worker).payload;
            let index = active.iter().copied().find(|i| *i < 2).unwrap();
            cli(&[
                "submit",
                &addresses[index],
                first_request.to_str().unwrap(),
                public_key,
                &certs[index],
            ]);
            assert!(
                cli(&[
                    "task-status",
                    &addresses[index],
                    first_request.to_str().unwrap(),
                    public_key,
                    &certs[index]
                ])
                .contains("state=succeeded")
            );
            assert_eq!(snapshot(&active_bases[0], worker).payload, before);
            println!("SOAK-PASS exact first request remains idempotent beyond receipt cache");
        }
        round += 1;
        if round.is_multiple_of(8) {
            let ids = windows
                .iter()
                .flatten()
                .map(|node| node.child.id().to_string())
                .collect::<Vec<_>>()
                .join(",");
            let output = Command::new("pwsh.exe")
                .creation_flags(0x08000000)
                .args(["-NoProfile", "-Command", &format!("Get-Process -Id {ids} | Select-Object Id,CPU,WorkingSet64 | ConvertTo-Json -Compress")])
                .output().unwrap();
            assert!(output.status.success());
            println!(
                "SOAK-METRICS elapsed_s={} completed={round} windows={} linux={}",
                started.elapsed().as_secs(),
                String::from_utf8(output.stdout).unwrap().trim(),
                worker.request(json!({"action":"metrics"}))
            );
        }
        thread::sleep(Duration::from_secs(2));
    }
    if let Some(index) = offline {
        restart(index, worker, windows, bases, &addresses, certs);
        for (saved, task) in missed {
            replay(index, &saved, worker, &addresses, certs, public_key);
            wait("soak final exact missed request durably completes", || {
                snapshot(&bases[index], worker)
                    .state
                    .task_succeeded(task.clone())
                    == Some(true)
            });
            fs::remove_file(saved).unwrap();
        }
        wait("soak final full state catch-up", || {
            let expected = snapshot(&bases[0], worker).payload;
            bases[1..]
                .iter()
                .all(|base| snapshot(base, worker).payload == expected)
        });
    }
    equal_shared(bases, worker);
    latencies.sort_unstable();
    println!(
        "SOAK-PASS elapsed_s={} transfers={round} client_observed_commit_p50_ms={} p95_ms={} max_ms={}",
        started.elapsed().as_secs(),
        latencies[latencies.len() / 2],
        latencies[(latencies.len() - 1) * 95 / 100],
        latencies.last().unwrap()
    );
}

fn replay(
    index: usize,
    request: &Path,
    worker: &mut LinuxWorker,
    addresses: &[String],
    certs: &[String],
    public_key: &str,
) {
    // Offline clients retry their exact signed request; nodes do not flood history.
    if index < 2 {
        cli(&[
            "submit",
            &addresses[index],
            request.to_str().unwrap(),
            public_key,
            &certs[index],
        ]);
    } else {
        let distro =
            std::env::var("SECOND_WSL_DISTRO").unwrap_or_else(|_| "Ubuntu-26.04".to_owned());
        let root = worker.request(json!({"action":"info"}));
        let copy = format!("{}/soak-replay.json", root.as_str().unwrap());
        worker.request(
            json!({"action":"copy", "source":wsl_path(&distro, request), "destination":copy}),
        );
        worker.cli(&[
            "submit",
            &addresses[index],
            &copy,
            public_key,
            &certs[index],
        ]);
    }
}

fn restart(
    index: usize,
    worker: &mut LinuxWorker,
    windows: &mut [Option<RunningNode>],
    bases: &[PathBuf],
    addresses: &[String],
    certs: &[String],
) {
    if index < 2 {
        windows[index] = Some(start_node(
            env!("CARGO_BIN_EXE_second"),
            addresses[index].parse().unwrap(),
            &bases[index],
            index as u64 + 1,
        ));
        assert_eq!(windows[index].as_ref().unwrap().certificate, certs[index]);
    } else {
        let identity = worker.start(
            index as u64 + 1,
            &addresses[index],
            bases[index].to_str().unwrap(),
        );
        assert_eq!(identity[5], certs[index]);
    }
}
