use crate::support::{self, bind_validator_runtime, key, peer_record, temp_base, validator_set};
use second::{
    CURRENT_PROTOCOL_VERSION, Operation, SecondState, StateStore, ValidatorId, ValidatorRegistry,
    ValidatorRuntimeKeys, ValidatorSetTransition,
};
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn rejected_allocation_does_not_poison_restart_or_later_issue() {
    let validators = validator_set(1, [1]);
    let account = support::account(91);
    let base = temp_base("rejected-allocation-restart");
    let store = StateStore::new(&base);
    store
        .initialize(&SecondState::genesis([account], 1), &validators)
        .unwrap();
    let bind = || {
        bind_validator_runtime(
            &store,
            ValidatorRuntimeKeys::new(ValidatorId::new(1), key(3), key(4)),
            support::default_validator_runtime_config(),
        )
    };
    let runtime = bind();
    let invalid = support::verified_task(
        1910,
        vec![Operation::Issue {
            account,
            count: u64::MAX,
        }],
    );
    let generation = store.load().unwrap().unwrap().generation;
    assert!(
        runtime
            .submit_legal_task(invalid.signed_task().clone())
            .is_err()
    );
    let snapshot = store.load().unwrap().unwrap();
    assert_eq!(
        snapshot.generation, generation,
        "rejected range must not enter the durable queue"
    );
    assert_eq!(snapshot.state.bound_request_digest(invalid.task_id()), None);
    drop(runtime);
    let runtime = Arc::new(bind());
    let good = support::verified_task(1911, vec![Operation::Issue { account, count: 1 }]);
    runtime
        .submit_legal_task(good.signed_task().clone())
        .unwrap();
    let worker = support::spawn_node_runtime(&runtime);
    let completed = support::progress::wait_for_progress(Duration::from_secs(5), || async {
        assert!(
            !worker.is_finished(),
            "rejected request killed the restarted node"
        );
        let snapshot = support::progress::snapshot(&store).await;
        (
            snapshot.state.task_succeeded(good.task_id()) == Some(true),
            snapshot.generation,
        )
    })
    .await;
    worker.abort();
    let _ = worker.await;
    if completed.is_err() {
        support::allocation_diagnostics::report(
            &runtime,
            &store,
            &base,
            ValidatorId::new(1),
            [good.task_id()],
        );
    }
    assert!(completed.is_ok(), "{completed:?}");
    let generation = store.load().unwrap().unwrap().generation;
    assert_eq!(
        runtime
            .submit_legal_task(good.signed_task().clone())
            .unwrap(),
        second::LegalTaskSubmissionOutcome::AlreadySucceeded
    );
    assert_eq!(store.load().unwrap().unwrap().generation, generation);
    drop(runtime);
    support::cleanup_node_runtime(store, base);
}

#[tokio::test]
async fn exhausted_competing_allocation_rejects_without_stopping_other_business() {
    let validators = validator_set(1, [1]);
    let account = support::account(92);
    let base = temp_base("allocation-frontier-exhausted");
    let store = StateStore::new(&base);
    store
        .initialize(&SecondState::genesis([account], u64::MAX - 2), &validators)
        .unwrap();
    let bind = || {
        bind_validator_runtime(
            &store,
            ValidatorRuntimeKeys::new(ValidatorId::new(1), key(3), key(4)),
            support::default_validator_runtime_config(),
        )
    };
    let runtime = bind();
    let tasks = [1920, 1921]
        .map(|id| support::verified_task(id, vec![Operation::Issue { account, count: 2 }]));
    for task in &tasks {
        runtime
            .submit_legal_task(task.signed_task().clone())
            .unwrap();
        assert_eq!(
            runtime
                .submit_legal_task(task.signed_task().clone())
                .unwrap(),
            second::LegalTaskSubmissionOutcome::AlreadyPending
        );
    }
    drop(runtime);
    let runtime = Arc::new(bind());
    let worker = support::spawn_node_runtime(&runtime);
    let ordinary = support::verified_task(
        1922,
        vec![Operation::RegisterAccount {
            account: support::account(93),
        }],
    );
    let completed = async {
        support::progress::wait_for_progress(Duration::from_secs(5), || async {
            assert!(!worker.is_finished(), "exhausted candidate killed the node");
            let snapshot = support::progress::snapshot(&store).await;
            (
                snapshot.state.next_currency_address() == u64::MAX,
                snapshot.generation,
            )
        })
        .await?;
        runtime
            .submit_legal_task(ordinary.signed_task().clone())
            .unwrap();
        support::progress::wait_for_progress(Duration::from_secs(5), || async {
            assert!(!worker.is_finished());
            let snapshot = support::progress::snapshot(&store).await;
            (
                snapshot.state.task_succeeded(ordinary.task_id()) == Some(true),
                snapshot.generation,
            )
        })
        .await
    }
    .await;
    worker.abort();
    let _ = worker.await;
    if completed.is_err() {
        support::allocation_diagnostics::report(
            &runtime,
            &store,
            &base,
            ValidatorId::new(1),
            tasks
                .iter()
                .map(|task| task.task_id())
                .chain([ordinary.task_id()]),
        );
    }
    assert!(completed.is_ok(), "{completed:?}");
    let state = store.load().unwrap().unwrap().state;
    assert_eq!(state.balance(account), 2);
    assert_eq!(
        tasks
            .iter()
            .filter(|task| state.task_succeeded(task.task_id()) == Some(true))
            .count(),
        1
    );
    let loser = tasks
        .iter()
        .find(|task| state.task_succeeded(task.task_id()) != Some(true))
        .unwrap();
    assert!(
        runtime
            .submit_legal_task(loser.signed_task().clone())
            .is_err()
    );
    drop(runtime);
    let runtime = Arc::new(bind());
    let after_restart = support::verified_task(
        1923,
        vec![Operation::RegisterAccount {
            account: support::account(96),
        }],
    );
    runtime
        .submit_legal_task(after_restart.signed_task().clone())
        .unwrap();
    let worker = support::spawn_node_runtime(&runtime);
    let completed = support::progress::wait_for_progress(Duration::from_secs(5), || async {
        assert!(
            !worker.is_finished(),
            "rejected candidate remained in restart queue"
        );
        let snapshot = support::progress::snapshot(&store).await;
        (
            snapshot.state.task_succeeded(after_restart.task_id()) == Some(true),
            snapshot.generation,
        )
    })
    .await;
    worker.abort();
    let _ = worker.await;
    assert!(completed.is_ok(), "{completed:?}");
    drop(runtime);
    support::cleanup_node_runtime(store, base);
}

#[tokio::test]
async fn certified_ranges_resume_after_crash_and_failed_business_never_reallocates() {
    let validators = validator_set(1, [1]);
    let account = support::account(94);
    let base = temp_base("certified-allocation-crash");
    let store = StateStore::new(&base);
    let mut state = SecondState::genesis([account], 1);
    store.initialize(&state, &validators).unwrap();
    let failed = support::verified_task(
        1930,
        vec![Operation::Issue {
            account: support::account(95),
            count: 1,
        }],
    );
    let good = support::verified_task(1931, vec![Operation::Issue { account, count: 2 }]);
    // Stop at the durable boundary: both QCs installed, no business prepared.
    let allocation = second::CurrencyAllocation::new(&failed, 1, 1).unwrap();
    let certificate = support::certificate_from_keys(
        allocation.finality_statement(),
        &validators,
        [(ValidatorId::new(1), key(4))],
    );
    store
        .install_currency_allocation(&allocation, &certificate)
        .unwrap();
    state = store.load().unwrap().unwrap().state;
    support::allocate_task(&store, &mut state, &good, 1, &validators).unwrap();
    assert_eq!(
        second::PreparedTaskBook::new(store.clone())
            .unwrap()
            .prepared_count(),
        0
    );
    let generation = store.load().unwrap().unwrap().generation;
    store
        .install_currency_allocation(&allocation, &certificate)
        .unwrap();
    assert_eq!(
        store.load().unwrap().unwrap().generation,
        generation,
        "QC replay must be a no-op even after the frontier advances"
    );
    let bind = || {
        bind_validator_runtime(
            &store,
            ValidatorRuntimeKeys::new(ValidatorId::new(1), key(3), key(4)),
            support::default_validator_runtime_config(),
        )
    };
    let runtime = Arc::new(bind());
    let worker = support::spawn_node_runtime(&runtime);
    let completed = support::progress::wait_for_progress(Duration::from_secs(5), || async {
        assert!(!worker.is_finished());
        let snapshot = support::progress::snapshot(&store).await;
        (
            snapshot.state.task_succeeded(good.task_id()) == Some(true),
            snapshot.generation,
        )
    })
    .await;
    worker.abort();
    let _ = worker.await;
    if completed.is_err() {
        support::allocation_diagnostics::report(
            &runtime,
            &store,
            &base,
            ValidatorId::new(1),
            [failed.task_id(), good.task_id()],
        );
    }
    assert!(completed.is_ok());
    assert!(
        runtime
            .submit_legal_task(failed.signed_task().clone())
            .is_err()
    );
    let snapshot = store.load().unwrap().unwrap();
    assert_eq!(snapshot.state.next_currency_address(), 4);
    assert_eq!(snapshot.state.balance(account), 2);
    assert!(
        !snapshot
            .state
            .currency_exists(second::CurrencyAddress::new(1))
    );
    assert!(
        snapshot
            .state
            .currency_exists(second::CurrencyAddress::new(2))
    );
    assert!(
        snapshot
            .state
            .currency_exists(second::CurrencyAddress::new(3))
    );
    drop(runtime);
    let runtime = Arc::new(bind());
    let later = support::verified_task(1932, vec![Operation::Issue { account, count: 1 }]);
    runtime
        .submit_legal_task(later.signed_task().clone())
        .unwrap();
    let worker = support::spawn_node_runtime(&runtime);
    let completed = support::progress::wait_for_progress(Duration::from_secs(5), || async {
        assert!(!worker.is_finished());
        let snapshot = support::progress::snapshot(&store).await;
        (
            snapshot.state.task_succeeded(later.task_id()) == Some(true),
            snapshot.generation,
        )
    })
    .await;
    worker.abort();
    let _ = worker.await;
    assert!(completed.is_ok());
    let state = store.load().unwrap().unwrap().state;
    assert_eq!(state.next_currency_address(), 5);
    assert_eq!(state.balance(account), 3);
    assert!(!state.currency_exists(second::CurrencyAddress::new(1)));
    drop(runtime);
    support::cleanup_node_runtime(store, base);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn competing_allocations_converge_after_opposite_submission_order_and_restart() {
    let validators = validator_set(1, 1..=4);
    let alice = support::account(81);
    let bob = support::account(82);
    let a = support::verified_task(
        1800,
        vec![Operation::Issue {
            account: alice,
            count: 2,
        }],
    );
    let b = support::verified_task(
        1801,
        vec![Operation::Issue {
            account: bob,
            count: 3,
        }],
    );
    let mut fixtures = Vec::new();
    for id in 1..=4 {
        let base = temp_base(&format!("opposite-allocation-{id}"));
        let store = StateStore::new(&base);
        store
            .initialize(&SecondState::genesis([alice, bob], 1), &validators)
            .unwrap();
        let runtime = bind_validator_runtime(
            &store,
            ValidatorRuntimeKeys::new(
                ValidatorId::new(id),
                key((id * 3) as u8),
                key((id * 3 + 1) as u8),
            ),
            support::default_validator_runtime_config(),
        );
        let tasks = if id % 2 == 0 { [&b, &a] } else { [&a, &b] };
        for task in tasks {
            runtime
                .submit_legal_task(task.signed_task().clone())
                .unwrap();
        }
        drop(runtime);
        let runtime = Arc::new(bind_validator_runtime(
            &store,
            ValidatorRuntimeKeys::new(
                ValidatorId::new(id),
                key((id * 3) as u8),
                key((id * 3 + 1) as u8),
            ),
            support::default_validator_runtime_config(),
        ));
        fixtures.push((runtime, store, base));
    }
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
                    .filter(|(other, _)| *other != index)
                    .map(|(_, record)| record.clone())
                    .collect(),
            )
        })
        .collect::<Vec<_>>();
    let complete = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if support::progress::snapshots(fixtures.iter().map(|(_, store, _)| store.clone()))
                .await
                .iter()
                .all(|snapshot| {
                    let state = &snapshot.state;
                    state.task_succeeded(a.task_id()) == Some(true)
                        && state.task_succeeded(b.task_id()) == Some(true)
                })
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    for worker in workers {
        worker.abort();
        let _ = worker.await;
    }
    let states = fixtures
        .iter()
        .map(|(_, store, _)| store.load().unwrap().unwrap().state)
        .collect::<Vec<_>>();
    if complete.is_err() {
        for (index, (runtime, store, base)) in fixtures.iter().enumerate() {
            support::allocation_diagnostics::report(
                runtime,
                store,
                base,
                ValidatorId::new(index as u64 + 1),
                [a.task_id(), b.task_id()],
            );
        }
    }
    assert!(complete.is_ok(), "allocator stalled");
    for state in &states {
        assert_eq!(
            state.public_currency_states(),
            states[0].public_currency_states()
        );
        assert_eq!(state.next_currency_address(), 6);
        assert_eq!(state.balance(alice), 2);
        assert_eq!(state.balance(bob), 3);
    }
    let payloads = fixtures
        .iter()
        .map(|(_, store, _)| {
            second::StateRecoveryPayload::from_persisted(&store.load().unwrap().unwrap())
                .unwrap()
                .encode_bytes()
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert!(payloads.iter().all(|payload| payload == &payloads[0]));
    for (runtime, store, base) in fixtures {
        drop(runtime);
        support::cleanup_node_runtime(store, base);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn membership_change_and_pending_issue_share_the_frontier_barrier() {
    let current = validator_set(1, 1..=4);
    let account = support::account(90);
    let task = support::verified_task(1900, vec![Operation::Issue { account, count: 1 }]);
    let late_account = support::account(92);
    let late = support::verified_task(
        1910,
        vec![Operation::RegisterAccount {
            account: late_account,
        }],
    );
    let registry = ValidatorRegistry::from_validator_set(&current).unwrap();
    let transition = ValidatorSetTransition::new(
        CURRENT_PROTOCOL_VERSION,
        &current,
        &registry,
        validator_set(2, 1..=4),
        Vec::new(),
        Vec::new(),
        1,
    )
    .unwrap();
    let mut fixtures = Vec::new();
    for id in 1..=4 {
        let base = temp_base(&format!("allocator-membership-{id}"));
        let store = StateStore::new(&base);
        store
            .initialize(&SecondState::genesis([account], 1), &current)
            .unwrap();
        let runtime = Arc::new(bind_validator_runtime(
            &store,
            ValidatorRuntimeKeys::new(
                ValidatorId::new(id),
                key((id * 3) as u8),
                key((id * 3 + 1) as u8),
            ),
            support::default_validator_runtime_config(),
        ));
        runtime
            .submit_legal_task(task.signed_task().clone())
            .unwrap();
        runtime
            .start_validator_set_transition_consensus(transition.clone())
            .unwrap();
        let generation = store.load().unwrap().unwrap().generation;
        // Collection is still open; a new local obligation must join the root.
        // The shared sealed-root regressions separately prove admission closes.
        runtime
            .submit_legal_task(late.signed_task().clone())
            .unwrap();
        let collected = store.load().unwrap().unwrap();
        assert!(collected.generation > generation);
        assert_eq!(
            collected.state.bound_request_digest(late.task_id()),
            Some(late.request_digest())
        );
        fixtures.push((runtime, store, base));
    }
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
                    .filter(|(other, _)| *other != index)
                    .map(|(_, record)| record.clone())
                    .collect(),
            )
        })
        .collect::<Vec<_>>();
    let complete = tokio::time::timeout(Duration::from_secs(20), async {
        let mut replanned = false;
        loop {
            if !replanned
                && support::progress::snapshots(fixtures.iter().map(|(_, store, _)| store.clone()))
                    .await
                    .iter()
                    .all(|snapshot| {
                        snapshot.validator_set.version() == 1
                            && snapshot.state.task_succeeded(task.task_id()) == Some(true)
                    })
            {
                // The allocation may win the first round while bootstrap connects.
                // A transition tied to that old frontier must be rebuilt, never
                // installed over the already consumed identity range.
                let fresh = ValidatorSetTransition::new(
                    CURRENT_PROTOCOL_VERSION,
                    &current,
                    &registry,
                    validator_set(2, 1..=4),
                    Vec::new(),
                    Vec::new(),
                    2,
                )
                .unwrap();
                for (runtime, _, _) in &fixtures {
                    runtime
                        .start_validator_set_transition_consensus(fresh.clone())
                        .unwrap();
                }
                replanned = true;
            }
            if support::progress::snapshots(fixtures.iter().map(|(_, store, _)| store.clone()))
                .await
                .iter()
                .all(|snapshot| {
                    snapshot.validator_set.version() == 2
                        && snapshot.state.task_succeeded(task.task_id()) == Some(true)
                        && snapshot.state.task_succeeded(late.task_id()) == Some(true)
                })
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    for worker in workers {
        worker.abort();
        let _ = worker.await;
    }
    if complete.is_err() {
        for (index, (runtime, store, base)) in fixtures.iter().enumerate() {
            support::allocation_diagnostics::report(
                runtime,
                store,
                base,
                ValidatorId::new(index as u64 + 1),
                [task.task_id()],
            );
        }
    }
    assert!(complete.is_ok(), "allocator epoch transition stalled");
    for (runtime, store, base) in fixtures {
        let snapshot = store.load().unwrap().unwrap();
        assert_eq!(snapshot.state.next_currency_address(), 2);
        assert_eq!(snapshot.state.balance(account), 1);
        assert!(snapshot.state.has_account(late_account));
        assert!(
            runtime
                .start_validator_set_transition_consensus(transition.clone())
                .is_err()
        );
        drop(runtime);
        support::cleanup_node_runtime(store, base);
    }
}
