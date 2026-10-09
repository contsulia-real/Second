use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::SigningKey;
use second::{AccountAddress, StateStore};
use serde_json::json;

use crate::cli_network_init::start_node;
use crate::support;

fn command(executable: &str, args: &[&str]) -> String {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let result = Command::new(executable).args(args).output().unwrap();
        if result.status.success() {
            return String::from_utf8(result.stdout).unwrap();
        }
        let error = String::from_utf8_lossy(&result.stderr);
        if error.contains("GovernanceRejected(Busy)") && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(200));
            continue;
        }
        panic!("{args:?}: {error}");
    }
}

fn wait(label: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(35);
    while !ready() {
        assert!(Instant::now() < deadline, "deadline: {label}");
        thread::sleep(Duration::from_millis(50));
    }
}

/// Continue the existing four-process fixture after all original nodes stop.
pub(crate) fn verify_reentry(
    executable: &str,
    addresses: &[SocketAddr],
    bases: &[PathBuf],
    deployment: &Path,
    recipient: AccountAddress,
    authorizer: &SigningKey,
) {
    let before = StateStore::new(&bases[0]).load().unwrap().unwrap();
    let old_set = before.validator_set.clone();
    let material: serde_json::Value = serde_json::from_slice(
        &fs::read(support::append_suffix(&bases[0], ".validator.keys.json")).unwrap(),
    )
    .unwrap();
    let old_seed: [u8; 32] = STANDARD
        .decode(
            material["consensus_private_keys_base64"][0]
                .as_str()
                .unwrap(),
        )
        .unwrap()
        .try_into()
        .unwrap();
    let mut healthy = (1..4)
        .map(|i| start_node(executable, addresses[i], &bases[i], i as u64 + 1))
        .collect::<Vec<_>>();
    let address = addresses[1].to_string();
    let cert = healthy[0].certificate.clone();
    command(
        executable,
        &[
            "recovery-checkpoint",
            &address,
            bases[1].to_str().unwrap(),
            &cert,
        ],
    );
    wait("v1 recovery proof", || {
        (1..4).all(|i| {
            StateStore::new(&bases[i])
                .load()
                .unwrap()
                .unwrap()
                .recovery_checkpoint_proof
                .is_some()
        })
    });
    let directory = deployment.join("recovered-validator-1");
    fs::create_dir(&directory).unwrap();
    let recovered_base = directory.join("second");
    for suffix in [".validator.keys.json", ".validator.json", ".bootstrap.json"] {
        fs::copy(
            support::append_suffix(&bases[0], suffix),
            support::append_suffix(&recovered_base, suffix),
        )
        .unwrap();
    }
    fs::copy(
        second::transport_identity_path(&bases[0]),
        second::transport_identity_path(&recovered_base),
    )
    .unwrap();
    command(
        executable,
        &[
            "recovery-install",
            &address,
            recovered_base.to_str().unwrap(),
            bases[1].to_str().unwrap(),
            &cert,
        ],
    );
    let restored = StateStore::new(&recovered_base).load().unwrap().unwrap();
    assert!(!restored.validator_safety_ready);
    assert_eq!(
        restored.state.balance(recipient),
        before.state.balance(recipient)
    );
    let rotation = directory.join("rotation.request");
    command(
        executable,
        &[
            "validator-rotate",
            recovered_base.to_str().unwrap(),
            "identity",
            rotation.to_str().unwrap(),
        ],
    );
    let plan = directory.join("transition.json");
    fs::write(
        &plan,
        serde_json::to_vec(&json!({"rotation_request_files":["rotation.request"]})).unwrap(),
    )
    .unwrap();
    let source = directory.join("transition.source");
    command(
        executable,
        &[
            "validator-transition-build",
            bases[1].to_str().unwrap(),
            plan.to_str().unwrap(),
            source.to_str().unwrap(),
        ],
    );
    command(
        executable,
        &[
            "validator-transition-submit",
            &address,
            bases[1].to_str().unwrap(),
            source.to_str().unwrap(),
            &cert,
        ],
    );
    let all_bases = std::iter::once(recovered_base.clone())
        .chain(bases[1..].iter().cloned())
        .collect::<Vec<_>>();
    wait("v2 membership while recovered node is offline", || {
        bases[1..].iter().all(|base| {
            StateStore::new(base)
                .load()
                .unwrap()
                .unwrap()
                .validator_set
                .version()
                == 2
        })
    });
    let intermediate_set = StateStore::new(&bases[1])
        .load()
        .unwrap()
        .unwrap()
        .validator_set;
    // Rotate another member while Validator 1 remains offline. Its V2 recovery
    // evidence must survive this extra transition without becoming voting authority.
    let other_rotation = directory.join("other-rotation.request");
    let source = directory.join("second-transition.source");
    command(
        executable,
        &[
            "validator-rotate",
            bases[3].to_str().unwrap(),
            "identity",
            other_rotation.to_str().unwrap(),
        ],
    );
    fs::write(
        &plan,
        serde_json::to_vec(&json!({
            "rotation_request_files": ["other-rotation.request"]
        }))
        .unwrap(),
    )
    .unwrap();
    command(
        executable,
        &[
            "validator-transition-build",
            bases[1].to_str().unwrap(),
            plan.to_str().unwrap(),
            source.to_str().unwrap(),
        ],
    );
    command(
        executable,
        &[
            "validator-transition-submit",
            &address,
            bases[1].to_str().unwrap(),
            source.to_str().unwrap(),
            &cert,
        ],
    );
    wait("v3 membership while recovered node is offline", || {
        bases[1..].iter().all(|base| {
            StateStore::new(base)
                .load()
                .unwrap()
                .unwrap()
                .validator_set
                .version()
                == 3
        })
    });
    // All proof providers restart: there is no completed-scope memory to relay.
    healthy.clear();
    healthy = (1..4)
        .map(|i| start_node(executable, addresses[i], &bases[i], i as u64 + 1))
        .collect();
    let mut recovered = Some(start_node(executable, addresses[0], &recovered_base, 1));
    wait(
        "late locked node pulls two durable transition proofs after every provider restarts",
        || {
            StateStore::new(&recovered_base)
                .load()
                .unwrap()
                .unwrap()
                .validator_set
                .version()
                == 3
        },
    );
    let transitioned = StateStore::new(&recovered_base).load().unwrap().unwrap();
    assert!(
        !transitioned.validator_safety_ready,
        "membership alone must not unlock signing"
    );
    assert_eq!(transitioned.minimum_signing_validator_set_version, 1);
    // Crash between membership finality and the V3 recovery proof. Resume from disk.
    drop(recovered.take());
    recovered = Some(start_node(executable, addresses[0], &recovered_base, 1));
    assert!(
        !StateStore::new(&recovered_base)
            .load()
            .unwrap()
            .unwrap()
            .validator_safety_ready
    );
    command(
        executable,
        &[
            "recovery-checkpoint",
            &address,
            bases[1].to_str().unwrap(),
            &cert,
        ],
    );
    wait("automatic safety recovery after restart", || {
        let state = StateStore::new(&recovered_base).load().unwrap().unwrap();
        state.validator_safety_ready && state.minimum_signing_validator_set_version == 3
    });
    // Reopen after unlock and exercise the real signer with the old key/set.
    drop(recovered.take());
    recovered = Some(start_node(executable, addresses[0], &recovered_base, 1));
    let reopened = StateStore::new(&recovered_base).load().unwrap().unwrap();
    let checkpoint =
        second::PublicCurrencyCheckpoint::new(1, 1, reopened.state.public_currency_summary());
    assert!(matches!(
        second::ValidatorSigner::new(
            second::ValidatorId::new(1),
            SigningKey::from_bytes(&old_seed),
            StateStore::new(&recovered_base)
        )
        .sign_public_checkpoint(&checkpoint, &old_set),
        Err(second::ValidatorSigningError::Persistence(
            second::PersistenceError::SigningFenceViolation {
                minimum_validator_set_version: 3,
                actual_validator_set_version: 1
            }
        ))
    ));
    let intermediate_checkpoint =
        second::PublicCurrencyCheckpoint::new(1, 2, reopened.state.public_currency_summary());
    let rotation_record = fs::read(support::append_suffix(
        &recovered_base,
        ".validator.rotation.keys",
    ))
    .unwrap();
    assert_eq!(rotation_record.len(), 149);
    let rotated_key = SigningKey::from_bytes(rotation_record[..32].try_into().unwrap());
    assert_eq!(
        rotated_key.verifying_key().to_bytes(),
        intermediate_set
            .validator(second::ValidatorId::new(1))
            .unwrap()
            .consensus_public_key()
    );
    assert!(matches!(
        second::ValidatorSigner::new(
            second::ValidatorId::new(1),
            rotated_key,
            StateStore::new(&recovered_base),
        )
        .sign_public_checkpoint(&intermediate_checkpoint, &intermediate_set),
        Err(second::ValidatorSigningError::Persistence(
            second::PersistenceError::SigningFenceViolation {
                minimum_validator_set_version: 3,
                actual_validator_set_version: 2,
            }
        ))
    ));
    // Stop Validator 4 so the recovered member is necessary for the 3-of-4 quorum.
    drop(healthy.pop());
    let request = directory.join("business.json");
    let wallet_config: serde_json::Value =
        serde_json::from_slice(&fs::read(deployment.join("wallet-network.json")).unwrap()).unwrap();
    let network_id: [u8; 32] = STANDARD
        .decode(wallet_config["network_id_base64"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let task_id = support::write_transaction_request_in_network(
        &request,
        9201,
        authorizer,
        vec![second::Operation::Issue {
            account: recipient,
            count: 1,
        }],
        json!([{"type":"issue","recipient":recipient.to_string(),"amount":1}]),
        network_id,
    );
    let recovered_cert = recovered.as_ref().unwrap().certificate.clone();
    command(
        executable,
        &[
            "submit",
            &addresses[0].to_string(),
            request.to_str().unwrap(),
            &STANDARD.encode(authorizer.verifying_key().to_bytes()),
            &recovered_cert,
        ],
    );
    let business_deadline = Instant::now() + Duration::from_secs(35);
    wait("business quorum including recovered Validator", || {
        let ready = all_bases[..3].iter().all(|base| {
            StateStore::new(base)
                .load()
                .unwrap()
                .unwrap()
                .state
                .task_succeeded(task_id.clone())
                == Some(true)
        });
        if !ready && Instant::now() >= business_deadline {
            for (index, base) in all_bases[..3].iter().enumerate() {
                let store = StateStore::new(base);
                let snapshot = store.load().unwrap().unwrap();
                let validator_id = second::ValidatorId::new(index as u64 + 1);
                let prepared = second::PreparedTaskBook::new(store.clone()).unwrap();
                for scope in [
                    second::ConsensusScope::PreparedTask(task_id.clone()),
                    second::ConsensusScope::CurrencyAllocation {
                        validator_set_version: snapshot.validator_set.version(),
                        start: before.state.next_currency_address(),
                    },
                ] {
                    let signer = second::ValidatorSigner::new(
                        validator_id,
                        support::key((index as u8 + 1) * 3 + 1),
                        store.clone(),
                    );
                    let finality_lock = signer.prepared_task_lock(task_id.clone()).unwrap();
                    let plan = prepared.prepared_plan_digest(task_id.clone());
                    eprintln!(
                        "reentry task decision: plan={plan:?} finality_lock={finality_lock:?}"
                    );
                    let local = store.bft_local_state(validator_id, &scope).unwrap();
                    let local = local
                        .as_ref()
                        .map(|state| (state.round(), state.locked_round(), state.locked_digest()));
                    eprintln!(
                        "reentry base={} scope={scope:?} frontier={} prepared={} succeeded={:?} safety={} fence={} bft={local:?}",
                        base.display(),
                        snapshot.state.next_currency_address(),
                        prepared.is_prepared(task_id.clone()),
                        snapshot.state.task_succeeded(task_id.clone()),
                        snapshot.validator_safety_ready,
                        snapshot.minimum_signing_validator_set_version
                    );
                }
            }
        }
        ready
    });
    for base in &all_bases[..3] {
        let state = StateStore::new(base).load().unwrap().unwrap();
        assert_eq!(
            state.state.balance(recipient),
            before.state.balance(recipient) + 1
        );
        assert_eq!(state.validator_set.version(), 3);
    }
    drop(recovered);
    drop(healthy);
}
