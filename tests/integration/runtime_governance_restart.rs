use crate::support::{self, bind_validator_runtime, key, temp_base, validator_set};
use second::*;
use std::{sync::Arc, time::Duration};

fn bind(store: &StateStore) -> Arc<NodeRuntime> {
    Arc::new(bind_validator_runtime(
        store,
        ValidatorRuntimeKeys::new(ValidatorId::new(1), key(3), key(4)),
        support::default_validator_runtime_config(),
    ))
}

#[tokio::test]
async fn admitted_recovery_survives_restart_before_votes_and_after_serial_lock() {
    for signed in [false, true] {
        let validators = validator_set(1, [1]);
        let base = temp_base("governance-recovery-restart");
        let store = StateStore::new(&base);
        store
            .initialize(&SecondState::genesis([], 1), &validators)
            .unwrap();
        let checkpoint = store.next_state_recovery_checkpoint().unwrap();
        let runtime = bind(&store);
        runtime
            .start_state_recovery_checkpoint_consensus(checkpoint.clone())
            .unwrap();
        let admitted_generation = store.load().unwrap().unwrap().generation;
        runtime
            .start_state_recovery_checkpoint_consensus(checkpoint.clone())
            .unwrap();
        assert_eq!(
            store.load().unwrap().unwrap().generation,
            admitted_generation
        );
        drop(runtime);
        if signed {
            support::mark_bft_finality_ready(
                &store,
                ValidatorId::new(1),
                ConsensusScope::StateRecoveryCheckpoint {
                    validator_set_version: 1,
                    serial: 1,
                },
                checkpoint.digest(),
                &validators,
                [(ValidatorId::new(1), key(4))],
            );
            ValidatorSigner::new(ValidatorId::new(1), key(4), store.clone())
                .sign_state_recovery_checkpoint(&checkpoint, &validators)
                .unwrap();
        }
        let restarted_store = StateStore::new(&base);
        assert_eq!(
            restarted_store.next_state_recovery_checkpoint().unwrap(),
            checkpoint
        );
        let premature =
            StateRecoveryCheckpoint::from_persisted(2, &restarted_store.load().unwrap().unwrap())
                .unwrap();
        assert!(
            restarted_store
                .state_recovery_bft_proposal_subject(&premature)
                .is_err()
        );
        let runtime = bind(&restarted_store);
        let worker = support::spawn_node_runtime(&runtime);
        let completed = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(proof) = restarted_store
                    .load()
                    .unwrap()
                    .unwrap()
                    .recovery_checkpoint_proof
                {
                    let certified = proof.verify_checkpoint(&validators).unwrap();
                    assert_eq!(certified.checkpoint(), &checkpoint);
                    assert_eq!(
                        restarted_store
                            .next_state_recovery_checkpoint()
                            .unwrap()
                            .serial(),
                        2
                    );
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        worker.abort();
        let _ = worker.await;
        drop(runtime);
        support::cleanup_node_runtime(restarted_store, base);
        completed.expect("admitted recovery did not resume its original serial and digest");
    }
}

#[tokio::test]
async fn admitted_membership_source_survives_restart_before_first_vote() {
    let validators = validator_set(1, [1]);
    let base = temp_base("governance-membership-restart");
    let store = StateStore::new(&base);
    store
        .initialize(&SecondState::genesis([], 1), &validators)
        .unwrap();
    let snapshot = store.load().unwrap().unwrap();
    let transition = ValidatorSetTransition::new(
        CURRENT_PROTOCOL_VERSION,
        &validators,
        &snapshot.validator_registry,
        validator_set(2, [1]),
        vec![],
        vec![],
        snapshot.state.next_currency_address(),
    )
    .unwrap();
    let runtime = bind(&store);
    runtime
        .start_validator_set_transition_consensus(transition.clone())
        .unwrap();
    drop(runtime);
    let restarted_store = StateStore::new(&base);
    let runtime = bind(&restarted_store);
    let worker = support::spawn_node_runtime(&runtime);
    let completed = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if restarted_store
                .load()
                .unwrap()
                .unwrap()
                .validator_set
                .version()
                == 2
            {
                assert!(runtime.drain_bft_consensus_events().unwrap().iter().any(|event|
                    matches!(event, BftConsensusEvent::CertifiedValidatorSetTransition(value)
                        if value.transition() == &transition)));
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    worker.abort();
    let _ = worker.await;
    drop(runtime);
    support::cleanup_node_runtime(restarted_store, base);
    completed.expect("admitted membership source did not resume");
}

#[tokio::test]
async fn changed_business_state_converges_after_restart_without_rebinding_signed_serial() {
    for signed in [false, true] {
        let validators = validator_set(1, [1]);
        let base = temp_base("recovery-changing-state");
        let store = StateStore::new(&base);
        let account = support::account(88);
        let mut state = SecondState::genesis([account], 1);
        store.initialize(&state, &validators).unwrap();
        let old = store.next_state_recovery_checkpoint().unwrap();
        let runtime = bind(&store);
        runtime
            .start_state_recovery_checkpoint_consensus(old.clone())
            .unwrap();
        drop(runtime);
        if signed {
            support::mark_bft_finality_ready(
                &store,
                ValidatorId::new(1),
                ConsensusScope::StateRecoveryCheckpoint {
                    validator_set_version: 1,
                    serial: 1,
                },
                old.digest(),
                &validators,
                [(ValidatorId::new(1), key(4))],
            );
            ValidatorSigner::new(ValidatorId::new(1), key(4), store.clone())
                .sign_state_recovery_checkpoint(&old, &validators)
                .unwrap();
        }
        let task = support::verified_task(8801, vec![Operation::Issue { account, count: 1 }]);
        let mut book = PreparedTaskBook::new(store.clone()).unwrap();
        support::allocate_task(&store, &mut state, &task, 0, &validators).unwrap();
        book.prepare(&mut state, &task, 0, &validators).unwrap();
        let certificate = support::certificate_from_keys(
            book.prepared_finality_statement(task.task_id()).unwrap(),
            &validators,
            [(ValidatorId::new(1), key(4))],
        );
        book.commit(&mut state, task.task_id(), &certificate)
            .unwrap();
        let replacement =
            StateRecoveryCheckpoint::from_persisted(1, &store.load().unwrap().unwrap()).unwrap();
        assert_ne!(replacement.digest(), old.digest());
        if signed {
            assert!(
                store
                    .state_recovery_bft_proposal_subject(&replacement)
                    .is_err()
            );
        }
        let restarted_store = StateStore::new(&base);
        let runtime = bind(&restarted_store);
        let worker = support::spawn_node_runtime(&runtime);
        let completed = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let snapshot = restarted_store.load().unwrap().unwrap();
                if let Some(proof) = snapshot.recovery_checkpoint_proof {
                    let certified = proof.verify_checkpoint(&validators).unwrap();
                    assert_eq!(certified.checkpoint().serial(), if signed { 2 } else { 1 });
                    certified
                        .verify_payload(
                            &StateRecoveryPayload::from_persisted(
                                &restarted_store.load().unwrap().unwrap(),
                            )
                            .unwrap(),
                            &validators,
                        )
                        .unwrap();
                    assert_eq!(snapshot.state.public_currency_summary().current_supply, 1);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        worker.abort();
        let _ = worker.await;
        if signed {
            assert!(runtime.drain_bft_consensus_events().unwrap().iter().any(|event|
                matches!(event, BftConsensusEvent::CertifiedStateRecoveryCheckpoint(value) if value.checkpoint() == &old)));
            assert!(
                ValidatorSigner::new(ValidatorId::new(1), key(4), restarted_store.clone())
                    .sign_state_recovery_checkpoint(&replacement, &validators)
                    .is_err()
            );
        }
        drop(runtime);
        support::cleanup_node_runtime(restarted_store, base);
        completed.expect("recovery did not converge to the changed shared state");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recovery_candidate_changes_and_historical_qc_catchup_converge_across_four_nodes() {
    for signed in [false, true] {
        let validators = validator_set(1, 1..=4);
        let account = support::account(89);
        let task = support::verified_task(8901, vec![Operation::Issue { account, count: 1 }]);
        let mut fixtures = Vec::new();
        for id in 1..=4 {
            let base = temp_base("recovery-quorum-changing-state");
            let store = StateStore::new(&base);
            let mut state = SecondState::genesis([account], 1);
            store.initialize(&state, &validators).unwrap();
            let old = store.next_state_recovery_checkpoint().unwrap();
            if id == 1 || (signed && id <= 3) {
                let runtime = bind_validator_runtime(
                    &store,
                    ValidatorRuntimeKeys::new(
                        ValidatorId::new(id),
                        key((id * 3) as u8),
                        key((id * 3 + 1) as u8),
                    ),
                    support::validator_runtime_config(BftTimeoutConfig::new(
                        Duration::from_secs(1),
                        Duration::from_secs(1),
                        Duration::from_secs(1),
                    )),
                );
                runtime
                    .start_state_recovery_checkpoint_consensus(old.clone())
                    .unwrap();
                drop(runtime);
                if signed {
                    support::mark_bft_finality_ready(
                        &store,
                        ValidatorId::new(id),
                        ConsensusScope::StateRecoveryCheckpoint {
                            validator_set_version: 1,
                            serial: 1,
                        },
                        old.digest(),
                        &validators,
                        (1..=3).map(|v| (ValidatorId::new(v), key((v * 3 + 1) as u8))),
                    );
                    ValidatorSigner::new(
                        ValidatorId::new(id),
                        key((id * 3 + 1) as u8),
                        store.clone(),
                    )
                    .sign_state_recovery_checkpoint(&old, &validators)
                    .unwrap();
                }
            }
            let mut book = PreparedTaskBook::new(store.clone()).unwrap();
            support::allocate_task(&store, &mut state, &task, 0, &validators).unwrap();
            book.prepare(&mut state, &task, 0, &validators).unwrap();
            let certificate = support::certificate_from_keys(
                book.prepared_finality_statement(task.task_id()).unwrap(),
                &validators,
                (1..=3).map(|v| (ValidatorId::new(v), key((v * 3 + 1) as u8))),
            );
            book.commit(&mut state, task.task_id(), &certificate)
                .unwrap();
            if id == 1 {
                assert!(store.state_recovery_bft_proposal_subject(&old).is_ok());
            }
            let runtime = Arc::new(bind_validator_runtime(
                &StateStore::new(&base),
                ValidatorRuntimeKeys::new(
                    ValidatorId::new(id),
                    key((id * 3) as u8),
                    key((id * 3 + 1) as u8),
                ),
                support::validator_runtime_config(BftTimeoutConfig::new(
                    Duration::from_secs(1),
                    Duration::from_secs(1),
                    Duration::from_secs(1),
                )),
            ));
            fixtures.push((runtime, store, base));
        }
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
        let completed = tokio::time::timeout(Duration::from_secs(55), async {
            loop {
                if fixtures.iter().all(|(_, store, _)| {
                    store
                        .load()
                        .unwrap()
                        .unwrap()
                        .recovery_checkpoint_proof
                        .as_ref()
                        .is_some_and(|proof| {
                            proof.checkpoint().serial() == if signed { 2 } else { 1 }
                        })
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            // The fourth node never admitted the historical candidate. It must publish
            // only the current payload after quorum evidence catches up its floor.
            let client = QuicClient::new(
                "127.0.0.1:0".parse().unwrap(),
                records[3].certificate_der(),
                QuicTransportIdentity::generate().unwrap(),
            )
            .unwrap();
            let peer = client.connect(records[3].address()).await.unwrap();
            let recovered =
                client_fetch_state_recovery(&peer, ValidatorId::new(1), &key(3), &validators)
                    .await
                    .unwrap();
            assert_eq!(
                recovered.checkpoint.checkpoint().serial(),
                if signed { 2 } else { 1 }
            );
            assert_eq!(
                recovered
                    .payload
                    .state()
                    .public_currency_summary()
                    .current_supply,
                1
            );
            peer.close();
            client.wait_idle().await;
        })
        .await;
        if completed.is_err() {
            eprintln!("signed mode: {signed}");
            for (index, (runtime, store, _)) in fixtures.iter().enumerate() {
                eprintln!(
                    "worker done: {}, bft: {:?}",
                    workers[index].is_finished(),
                    store
                        .bft_local_state(
                            ValidatorId::new(index as u64 + 1),
                            &ConsensusScope::StateRecoveryCheckpoint {
                                validator_set_version: 1,
                                serial: 1
                            }
                        )
                        .unwrap()
                );
                let snapshot = store.load().unwrap().unwrap();
                eprintln!(
                    "current proof: {:?}",
                    snapshot
                        .recovery_checkpoint_proof
                        .as_ref()
                        .map(|p| p.checkpoint().serial())
                );
                eprintln!(
                    "recovery events: {:?}",
                    runtime.drain_bft_consensus_events().unwrap()
                );
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
        completed.expect("four-node recovery did not converge after changing shared state");
    }
}
