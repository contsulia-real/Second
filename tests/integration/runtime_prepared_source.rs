use crate::support::{self, bind_validator_runtime, key, peer_record, temp_base, validator_set};
use second::{
    BftConsensusEvent, Operation, PreparedTaskBook, SecondState, StateStore, ValidatorId,
    ValidatorRuntimeKeys,
};
use std::{sync::Arc, time::Duration};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn private_pull_commits_remote_frozen_currency_instead_of_local_default() {
    run_frozen_source(false, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn same_request_with_two_frozen_currency_plans_converges_without_releasing_old_votes() {
    run_frozen_source(true, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn frozen_variants_blocked_by_competing_task_release_only_after_certified_abort() {
    run_frozen_source(true, true).await;
}

async fn run_frozen_source(variants: bool, blocked: bool) {
    let validators = validator_set(1, 1..=4);
    let alice = support::account(121);
    let bob = support::account(122);
    let setup = support::verified_task(
        3200,
        vec![
            Operation::RegisterPaymentAddress {
                address: support::payment_address(alice),
                account: alice,
            },
            Operation::RegisterPaymentAddress {
                address: support::payment_address(bob),
                account: bob,
            },
            Operation::Issue {
                account: alice,
                count: 2,
            },
        ],
    );
    let transfer = |id| {
        support::verified_task(
            id,
            vec![Operation::Transfer {
                source: support::payment_address(alice),
                destination: support::payment_address(bob),
                amount: 1,
            }],
        )
    };
    let blocker = transfer(3201);
    let task = transfer(3202);
    let competing = transfer(3203);
    let mut fixtures = Vec::new();
    let mut expected_digest = None;
    let mut initial_digests = std::collections::BTreeSet::new();
    for id in 1..=4 {
        let base = temp_base(&format!("frozen-private-pull-{id}"));
        let store = StateStore::new(&base);
        let mut state = SecondState::genesis([alice, bob], 1);
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
        if variants {
            book.prepare(&mut state, &blocker, 1, &validators).unwrap();
            if id % 2 == 1 {
                book.prepare(&mut state, &task, 1, &validators).unwrap();
            }
            book.cancel(blocker.task_id()).unwrap();
            if id % 2 == 0 {
                state = store.load().unwrap().unwrap().state;
                book.prepare(&mut state, &task, 1, &validators).unwrap();
            }
            initial_digests.insert(book.prepared_plan_digest(task.task_id()).unwrap());
            if blocked {
                book.prepare(&mut state, &competing, 1, &validators)
                    .unwrap();
                assert_eq!(book.claimed_currency_count(), 2);
            }
        } else if id == 2 {
            // Only this node has seen the blocker, so its target selects currency 2.
            book.prepare(&mut state, &blocker, 1, &validators).unwrap();
            book.prepare(&mut state, &task, 1, &validators).unwrap();
            expected_digest = Some(book.prepared_plan_digest(task.task_id()).unwrap());
            book.cancel(blocker.task_id()).unwrap();
        }
        let runtime = Arc::new(bind_validator_runtime(
            &store,
            ValidatorRuntimeKeys::new(
                ValidatorId::new(id),
                key((id * 3) as u8),
                key((id * 3 + 1) as u8),
            ),
            if variants {
                support::validator_runtime_config(second::BftTimeoutConfig::new(
                    Duration::from_millis(500),
                    Duration::from_millis(500),
                    Duration::from_millis(500),
                ))
            } else {
                support::default_validator_runtime_config()
            },
        ));
        fixtures.push((runtime, store, base));
    }
    if variants {
        assert_eq!(
            initial_digests.len(),
            2,
            "fixture must contain two genuinely different valid plans"
        );
    }
    let competing_abort = blocked.then(|| {
        fixtures[0]
            .1
            .prepared_abort_statement(&competing.task_id())
            .unwrap()
    });
    let records = fixtures
        .iter()
        .map(|(runtime, _, _)| peer_record(runtime))
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
                    .filter(|(candidate, _)| *candidate != index)
                    .map(|(_, record)| record.clone())
                    .collect(),
            )
        })
        .collect::<Vec<_>>();
    let mut certificates = vec![None; 4];
    // Draining the runtime queue must not erase the evidence needed on timeout.
    let mut recent_events = (0..4)
        .map(|_| std::collections::VecDeque::new())
        .collect::<Vec<_>>();
    let completed =
        tokio::time::timeout(Duration::from_secs(if variants { 30 } else { 20 }), async {
            loop {
                for (index, (runtime, _, _)) in fixtures.iter().enumerate() {
                    for event in runtime.drain_bft_consensus_events().unwrap() {
                        if let BftConsensusEvent::CertifiedPreparedTask {
                            task_id,
                            certificate,
                        } = event
                        {
                            if task_id == competing.task_id() {
                                assert_eq!(Some(certificate.statement()), competing_abort);
                                continue;
                            }
                            assert_eq!(task_id, task.task_id());
                            certificate.verify(&validators).unwrap();
                            if let Some(expected) = expected_digest {
                                assert_eq!(certificate.statement().subject_digest(), expected);
                            } else {
                                expected_digest = Some(certificate.statement().subject_digest());
                            }
                            certificates[index] = Some(certificate);
                        } else {
                            let history = &mut recent_events[index];
                            if history.len() == 16 {
                                history.pop_front();
                            }
                            history.push_back(event);
                        }
                    }
                }
                if certificates.iter().all(Option::is_some) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
    if completed.is_err() {
        let diagnostic = fixtures
            .iter()
            .enumerate()
            .map(|(index, (runtime, store, base))| {
                let scope = second::ConsensusScope::PreparedTask(task.task_id());
                let local_states = (1..=4)
                    .map(|id| {
                        let id = ValidatorId::new(id);
                        let local = store.bft_local_state(id, &scope).unwrap();
                        (
                            id,
                            local.map(|local| {
                                (
                                    local.round(),
                                    local.locked_round(),
                                    local.locked_digest(),
                                    local.valid_prevote_qc().map(|qc| qc.statement().clone()),
                                )
                            }),
                        )
                    })
                    .collect::<Vec<_>>();
                (
                    base,
                    workers[index].is_finished(),
                    runtime.connected_validator_ids(),
                    store.load().map(|snapshot| {
                        snapshot.map(|snapshot| {
                            (
                                snapshot.state.task_succeeded(task.task_id()),
                                snapshot.state.task_cancelled(task.task_id()),
                            )
                        })
                    }),
                    store.prepared_bft_proposal_subject(task.task_id()),
                    local_states,
                    &recent_events[index],
                    runtime.drain_bft_consensus_events(),
                )
            })
            .collect::<Vec<_>>();
        panic!("frozen source private pull stalled: {diagnostic:?}");
    }
    let mut final_currency_state = None;
    for (_, store, _) in &fixtures {
        let snapshot = store.load().unwrap().unwrap();
        let state = &snapshot.state;
        assert_eq!(state.task_succeeded(task.task_id()), Some(true));
        assert_eq!(state.balance(alice), 1);
        assert_eq!(state.balance(bob), 1);
        assert_eq!(state.next_currency_address(), 3);
        if blocked {
            assert!(state.task_cancelled(competing.task_id()));
        }
        if variants {
            let shared = second::StateRecoveryPayload::from_persisted(&snapshot)
                .unwrap()
                .encode_bytes()
                .unwrap();
            if let Some(expected) = &final_currency_state {
                assert_eq!(&shared, expected);
            } else {
                final_currency_state = Some(shared);
            }
        }
    }
    for worker in workers {
        worker.abort();
        let _ = worker.await;
    }
    for (runtime, store, base) in fixtures {
        drop(runtime);
        support::cleanup_node_runtime(store, base);
    }
}
