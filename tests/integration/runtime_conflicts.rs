//! Real QUIC opposite-order resource contention and certified release.
use crate::support::{self, bind_validator_runtime, key, peer_record, temp_base, validator_set};
use second::{
    Operation, PreparedTaskBook, SecondState, StateStore, ValidatorId, ValidatorRuntimeKeys,
};
use std::{sync::Arc, time::Duration};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn split_resource_claims_converge_by_certified_abort_and_preserve_terminal_results_after_restart()
 {
    run_resource_conflicts(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn existing_commit_qc_outranks_lower_task_id_without_granting_witness_commit_rights() {
    run_resource_conflicts(true).await;
}

async fn run_resource_conflicts(protected: bool) {
    let validators = validator_set(1, 1..=4);
    let alice = support::account(110);
    let bob = support::account(111);
    let charlie = support::account(114);
    let register = support::account(112);
    let payment = support::payment_address(bob);
    let retirement_payment = second::PaymentAddress::from_bytes([115; 32]);
    let setup = support::verified_task(
        3100,
        vec![
            Operation::RegisterPaymentAddress {
                address: support::payment_address(alice),
                account: alice,
            },
            Operation::RegisterPaymentAddress {
                address: payment,
                account: bob,
            },
            Operation::RegisterPaymentAddress {
                address: retirement_payment,
                account: bob,
            },
            Operation::Issue {
                account: alice,
                count: 1,
            },
            Operation::RegisterPaymentAddress {
                address: support::payment_address(charlie),
                account: charlie,
            },
        ],
    );
    let pairs = [
        [
            support::verified_task(
                3101,
                vec![Operation::Transfer {
                    source: support::payment_address(alice),
                    destination: payment,
                    amount: 1,
                }],
            ),
            support::verified_task(
                3102,
                vec![Operation::Transfer {
                    source: support::payment_address(alice),
                    destination: support::payment_address(charlie),
                    amount: 1,
                }],
            ),
        ],
        [
            support::verified_task(3103, vec![Operation::RegisterAccount { account: register }]),
            support::verified_task(3104, vec![Operation::RegisterAccount { account: register }]),
        ],
        [
            support::verified_task(
                3105,
                vec![Operation::RetirePaymentAddress {
                    address: retirement_payment,
                }],
            ),
            support::verified_task(
                3106,
                vec![Operation::RetirePaymentAddress {
                    address: retirement_payment,
                }],
            ),
        ],
    ];
    let independent = support::verified_task(
        3107,
        vec![Operation::RegisterAccount {
            account: support::account(113),
        }],
    );
    let mut fixtures = Vec::new();
    for id in 1..=4 {
        let base = temp_base(&format!("split-resource-{id}"));
        let store = StateStore::new(&base);
        let mut state = SecondState::genesis([alice, bob, charlie], 1);
        store.initialize(&state, &validators).unwrap();
        support::allocate_task(&store, &mut state, &setup, 1, &validators).unwrap();
        let mut book = PreparedTaskBook::new(store.clone()).unwrap();
        book.prepare(&mut state, &setup, 1, &validators).unwrap();
        let certificate = support::certificate_from_keys(
            book.prepared_finality_statement(setup.task_id()).unwrap(),
            &validators,
            (1..=3).map(|signer| (ValidatorId::new(signer), key((signer * 3 + 1) as u8))),
        );
        book.commit(&mut state, setup.task_id(), &certificate)
            .unwrap();
        let runtime = Arc::new(bind_validator_runtime(
            &store,
            ValidatorRuntimeKeys::new(
                ValidatorId::new(id),
                key((id * 3) as u8),
                key((id * 3 + 1) as u8),
            ),
            support::validator_runtime_config(second::BftTimeoutConfig::new(
                Duration::from_millis(500),
                Duration::from_millis(500),
                Duration::from_millis(500),
            )),
        ));
        for pair in &pairs {
            let choice = if protected {
                usize::from(id != 1)
            } else {
                (id % 2) as usize
            };
            runtime
                .submit_legal_task(pair[choice].signed_task().clone())
                .unwrap();
            if protected && id != 1 {
                let subject = store
                    .prepared_bft_proposal_subject(pair[1].task_id())
                    .unwrap();
                let qc = support::bft_qc(
                    subject.scope().clone(),
                    0,
                    second::BftPhase::Prevote,
                    second::BftValue::Digest(subject.digest()),
                    2..=4,
                    &validators,
                );
                let mut driver = second::BftDriver::new(
                    second::ValidatorSigner::new(
                        ValidatorId::new(id),
                        key((id * 3 + 1) as u8),
                        store.clone(),
                    ),
                    store.clone(),
                    validators.clone(),
                    subject.scope().clone(),
                )
                .unwrap();
                driver.register_subject(&subject).unwrap();
                driver.accept_quorum_certificate(&qc).unwrap();
            }
            assert_eq!(
                runtime
                    .submit_legal_task(pair[1 - choice].signed_task().clone())
                    .unwrap(),
                second::LegalTaskSubmissionOutcome::AlreadyPending
            );
        }
        fixtures.push((runtime, store, base));
    }
    if protected {
        for (index, (runtime, store, _)) in fixtures.iter_mut().enumerate() {
            let id = index as u64 + 1;
            *runtime = Arc::new(bind_validator_runtime(
                store,
                ValidatorRuntimeKeys::new(
                    ValidatorId::new(id),
                    key((id * 3) as u8),
                    key((id * 3 + 1) as u8),
                ),
                support::validator_runtime_config(second::BftTimeoutConfig::new(
                    Duration::from_millis(500),
                    Duration::from_millis(500),
                    Duration::from_millis(500),
                )),
            ));
        }
    }
    // Two online validators can admit witnesses but cannot form the quorum of
    // three. Restart after that durable admission, before any resource release.
    if !protected {
        let initial_records = fixtures
            .iter()
            .take(2)
            .map(|(runtime, _, _)| peer_record(runtime))
            .collect::<Vec<_>>();
        let initial_workers = fixtures
            .iter()
            .take(2)
            .enumerate()
            .map(|(index, (runtime, _, _))| {
                support::spawn_node_runtime_with_bootstrap(
                    runtime,
                    initial_records
                        .iter()
                        .enumerate()
                        .filter(|(other, _)| *other != index)
                        .map(|(_, record)| record.clone())
                        .collect(),
                )
            })
            .collect::<Vec<_>>();
        tokio::time::timeout(Duration::from_secs(40), async {
            loop {
                if pairs.iter().all(|pair| {
                    fixtures[1]
                        .1
                        .bft_local_state(
                            ValidatorId::new(2),
                            &second::ConsensusScope::PreparedTask(pair[1].task_id()),
                        )
                        .unwrap()
                        .is_some()
                }) {
                    break;
                }
                assert!(initial_workers.iter().all(|worker| !worker.is_finished()));
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("Abort-only witnesses must be durable before restart");
        for worker in initial_workers {
            worker.abort();
            let _ = worker.await;
        }
        for (index, (runtime, store, _)) in fixtures.iter_mut().enumerate() {
            let snapshot = store.load().unwrap().unwrap();
            assert!(
                pairs
                    .iter()
                    .flatten()
                    .all(|task| !snapshot.state.task_cancelled(task.task_id()))
            );
            assert_eq!(
                PreparedTaskBook::new(store.clone())
                    .unwrap()
                    .claimed_currency_count(),
                1
            );
            let id = index as u64 + 1;
            *runtime = Arc::new(bind_validator_runtime(
                store,
                ValidatorRuntimeKeys::new(
                    ValidatorId::new(id),
                    key((id * 3) as u8),
                    key((id * 3 + 1) as u8),
                ),
                support::validator_runtime_config(second::BftTimeoutConfig::new(
                    Duration::from_millis(500),
                    Duration::from_millis(500),
                    Duration::from_millis(500),
                )),
            ));
        }
    }
    let records = fixtures
        .iter()
        .map(|(runtime, _, _)| peer_record(runtime))
        .collect::<Vec<_>>();
    let mut workers = fixtures
        .iter()
        .enumerate()
        .filter(|(index, _)| !protected || *index != 0)
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
    if protected {
        // The late resource holder misses all early Commit QCs. The completed
        // quorum must recover it using only its final certificates.
        tokio::time::timeout(Duration::from_secs(90), async {
            loop {
                if support::progress::snapshots(
                    fixtures.iter().skip(1).map(|(_, store, _)| store.clone()),
                )
                .await
                .iter()
                .all(|snapshot| {
                    pairs.iter().all(|pair| {
                        snapshot.state.task_succeeded(pair[1].task_id()) == Some(true)
                            && snapshot.state.task_cancelled(pair[0].task_id())
                    })
                }) {
                    break;
                }
                assert!(workers.iter().all(|worker| !worker.is_finished()));
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the existing Commit QC quorum must finalize before the resource holder joins");
        workers.push(support::spawn_node_runtime_with_bootstrap(
            &fixtures[0].0,
            records.iter().skip(1).cloned().collect(),
        ));
    }
    for (runtime, _, _) in &fixtures {
        runtime
            .submit_legal_task(independent.signed_task().clone())
            .unwrap();
    }
    // Late witnesses enter their own durable round. Allow a full proposer
    // rotation for Abort, followed by the newly unblocked Commit rotation.
    let ready = tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            if support::progress::snapshots(fixtures.iter().map(|(_, store, _)| store.clone()))
                .await
                .iter()
                .all(|snapshot| {
                    snapshot.state.task_succeeded(independent.task_id()) == Some(true)
                        && pairs.iter().all(|pair| {
                            snapshot
                                .state
                                .task_succeeded(pair[usize::from(protected)].task_id())
                                == Some(true)
                                && snapshot
                                    .state
                                    .task_cancelled(pair[usize::from(!protected)].task_id())
                        })
                })
            {
                break;
            }
            assert!(workers.iter().all(|worker| !worker.is_finished()));
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    if ready.is_err() {
        for (index, (runtime, store, _)) in fixtures.iter().enumerate() {
            let snapshot = store.load().unwrap().unwrap();
            eprintln!(
                "node={} peers={:?} contexts={:?} events={:?}",
                index + 1,
                runtime.connected_validator_ids(),
                snapshot.state.task_succeeded(independent.task_id()),
                runtime.drain_bft_consensus_events().unwrap()
            );
            for pair in &pairs {
                for task in pair {
                    eprintln!(
                        "task={} succeeded={:?} cancelled={} bft={:?}",
                        task.task_id(),
                        snapshot.state.task_succeeded(task.task_id()),
                        snapshot.state.task_cancelled(task.task_id()),
                        store
                            .bft_local_state(
                                ValidatorId::new((index + 1) as u64),
                                &second::ConsensusScope::PreparedTask(task.task_id())
                            )
                            .unwrap()
                    );
                }
            }
        }
    }
    assert!(
        ready.is_ok(),
        "each conflict must produce one Commit and one certified Abort"
    );
    for worker in workers {
        worker.abort();
        let _ = worker.await;
    }
    for (_, store, base) in &fixtures {
        let restarted = StateStore::new(base);
        let snapshot = restarted.load().unwrap().unwrap();
        assert_eq!(snapshot.state.balance(alice), 0);
        assert_eq!(snapshot.state.balance(bob), u64::from(!protected));
        assert_eq!(snapshot.state.balance(charlie), u64::from(protected));
        assert!(snapshot.state.has_account(register));
        assert_eq!(snapshot.state.next_currency_address(), 2);
        assert_eq!(
            PreparedTaskBook::new(restarted.clone())
                .unwrap()
                .claimed_currency_count(),
            0
        );
        for pair in &pairs {
            assert_eq!(
                snapshot
                    .state
                    .task_succeeded(pair[usize::from(protected)].task_id()),
                Some(true)
            );
            assert!(
                snapshot
                    .state
                    .task_cancelled(pair[usize::from(!protected)].task_id())
            );
        }
        assert_eq!(
            store.load().unwrap().unwrap().generation,
            snapshot.generation
        );
    }
}
