//! Real authenticated source exchange and membership BFT with distinct local duties.
use super::*;

#[tokio::test]
async fn cold_membership_request_collects_distinct_duties_and_switches_four_nodes() {
    check_cold_membership(false, 4).await;
}

#[tokio::test]
async fn cold_running_nodes_collect_membership_and_finish_distinct_business() {
    check_cold_membership(true, 4).await;
}

#[tokio::test]
async fn cold_collection_switches_online_quorum_without_waiting_for_offline_member() {
    check_cold_membership(false, 3).await;
}

async fn check_cold_membership(running: bool, online: usize) {
    let validators = validator_set();
    let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
    let initial = SecondState::genesis([], 1);
    let mut stores = Vec::new();
    let mut tasks = Vec::new();
    for id in 1..=4 {
        let (store, base) = temp_store();
        store.initialize(&initial, &validators).unwrap();
        let task = crate::test_helpers::sign(
            LegalTaskPayload::new(
                TaskId::parse(&format!("automatic-handoff-local-{id}")).unwrap(),
                1,
                None,
                vec![Operation::RegisterAccount {
                    account: crate::test_helpers::account(210 + id as u8),
                }],
            ),
            &key(9),
        )
        .unwrap()
        .verify(&authorizers)
        .unwrap();
        PreparedTaskBook::new(store.clone())
            .unwrap()
            .prepare(&mut initial.clone(), &task, 1, &validators)
            .unwrap();
        stores.push((store, base));
        tasks.push(task);
    }
    let bind = |id: u64, store: &StateStore| {
        std::sync::Arc::new(
            NodeRuntime::bind_loaded(
                "127.0.0.1:0".parse().unwrap(),
                store,
                store.load().unwrap().unwrap(),
                NodeRuntimeCapabilities::default().with_validator(
                    ValidatorRuntimeKeys::new(
                        ValidatorId::new(id),
                        key((id * 3) as u8),
                        key((id * 3 + 1) as u8),
                    ),
                    ValidatorRuntimeConfig::new(
                        authorizers.clone(),
                        BftTimeoutConfig::new(
                            Duration::from_secs(1),
                            Duration::from_secs(1),
                            Duration::from_secs(1),
                        ),
                        || 1,
                    ),
                ),
            )
            .unwrap(),
        )
    };
    let snapshot = stores[0].0.load().unwrap().unwrap();
    let transition = ValidatorSetTransition::new(
        1,
        &validators,
        &snapshot.validator_registry,
        ValidatorSet::new(2, validators.credentials().cloned()).unwrap(),
        vec![],
        vec![],
        1,
    )
    .unwrap();
    if !running && online == 4 {
        // A former allocation round at this same frontier must not make a new
        // collection immediately eligible for sealing, including after restart.
        for (index, (store, _)) in stores.iter().enumerate() {
            for round in 0..4 {
                crate::prepared::tests::advance_nil_round(
                    store,
                    &validators,
                    ValidatorId::new(index as u64 + 1),
                    transition.scope(),
                    round,
                );
            }
        }
    }
    // Lose the first announcement, then recreate the requesting runtime from disk.
    let first = bind(1, &stores[0].0);
    first
        .start_validator_set_transition_consensus(transition)
        .unwrap();
    let collecting = stores[0].0.load().unwrap().unwrap();
    assert!(
        collecting.pending_governance.values().all(|pending| {
            matches!(
                pending,
                crate::persistence::PendingGovernance::CollectingTransition(_)
            )
        }),
        "operator submission sealed its private root before source exchange"
    );
    assert!(collecting.validator_vote_locks.is_empty());
    drop(first);
    let nodes: Vec<_> = stores
        .iter()
        .take(online)
        .enumerate()
        .map(|(index, (store, _))| bind(index as u64 + 1, store))
        .collect();
    let (records, workers) = if running {
        let records = nodes
            .iter()
            .map(|node| node.local_peer_record().unwrap().clone())
            .collect::<Vec<_>>();
        let workers = nodes
            .iter()
            .map(|node| {
                let node = node.clone();
                tokio::spawn(async move {
                    node.run(&[]).await.unwrap();
                })
            })
            .collect::<Vec<_>>();
        (records, workers)
    } else {
        connect_handoff_nodes(&nodes)
    };
    // A transport may disappear before sending its first request. This must
    // close one connection, not take the membership listener down with it.
    let client = crate::QuicClient::new(
        "127.0.0.1:0".parse().unwrap(),
        records[0].certificate_der(),
        crate::QuicTransportIdentity::generate().unwrap(),
    )
    .unwrap();
    let abandoned = client.connect(records[0].address()).await.unwrap();
    abandoned.close();
    client.wait_idle().await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        workers.iter().all(|worker| !worker.is_finished()),
        "an abandoned first request killed the membership listener"
    );
    for (index, node) in nodes.iter().enumerate() {
        for (other, record) in records.iter().enumerate() {
            if index != other {
                node.dial_validator_bft(record).await.unwrap();
            }
        }
        if !running {
            node.resume_governance().unwrap();
        }
    }
    // Manual driving must include the runner's bounded source retry deadline;
    // consensus timeouts alone do not retry an exhausted provider set.
    let mut source_retry_at = tokio::time::Instant::now() + Duration::from_secs(1);
    let completed = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let retry_sources = tokio::time::Instant::now() >= source_retry_at;
            for node in nodes.iter().filter(|_| !running) {
                let node = node.clone();
                // Match the runner: durable work must not block this executor's
                // QUIC listeners while source bodies and votes are in flight.
                tokio::task::spawn_blocking(move || {
                    let bft = node.validator_bft.as_ref().unwrap();
                    let incoming = node.process_governance_bft_sources(bft.drain_inbound());
                    let incoming = node.process_prepared_task_sync(incoming).unwrap();
                    node.advance_transition_collections().unwrap();
                    let output = bft.consensus().drive(incoming, tokio::time::Instant::now());
                    for message in output.outbound {
                        assert!(bft.broadcast(&message).is_empty());
                    }
                    if output.validator_set_changed {
                        bft.refresh_authority().unwrap();
                    }
                    bft.retry_prepared_task_sync(retry_sources);
                    if retry_sources {
                        node.announce_pending_transition_sources().unwrap();
                    }
                })
                .await
                .unwrap();
            }
            if retry_sources {
                source_retry_at = tokio::time::Instant::now() + Duration::from_secs(1);
            }
            if stores.iter().take(online).all(|(store, _)| {
                let snapshot = store.load_shared().unwrap().unwrap();
                snapshot.validator_set.version() == 2
                    && (!running
                        || tasks.iter().all(|task| {
                            snapshot.state.task_succeeded(task.task_id()) == Some(true)
                        }))
            }) {
                break;
            }
            assert!(workers.iter().all(|worker| !worker.is_finished()));
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    if completed.is_err() {
        let status = stores
            .iter()
            .map(|(store, _)| {
                let snapshot = store.load_shared().unwrap().unwrap();
                (
                    snapshot.validator_set.version(),
                    snapshot.state.business.accounts.len(),
                    tasks
                        .iter()
                        .filter(|task| snapshot.state.task_succeeded(task.task_id()) == Some(true))
                        .count(),
                    snapshot
                        .state
                        .protocol
                        .task_handoff
                        .as_ref()
                        .map(|handoff| handoff.plans.len()),
                    snapshot.pending_governance.len(),
                )
            })
            .collect::<Vec<_>>();
        eprintln!(
            "membership status (version, accounts, completed, inherited, pending): {status:?}"
        );
        for (index, node) in nodes.iter().enumerate() {
            let snapshot = stores[index].0.load_shared().unwrap().unwrap();
            let duties = |digest: [u8; 32]| {
                snapshot
                    .pending_governance
                    .get(&digest)
                    .and_then(crate::persistence::PendingGovernance::transition)
                    .and_then(|transition| transition.handoff.as_ref())
                    .map(|handoff| handoff.plans.len() + handoff.requests.len())
            };
            let candidates = snapshot
                .pending_governance
                .values()
                .filter_map(crate::persistence::PendingGovernance::transition)
                .filter_map(|transition| transition.handoff.as_ref())
                .map(|handoff| handoff.plans.len() + handoff.requests.len())
                .collect::<Vec<_>>();
            let voting = snapshot
                .bft_local_states
                .iter()
                .filter(|((_, scope), _)| {
                    matches!(scope, ConsensusScope::CurrencyAllocation { .. })
                })
                .map(|(_, state)| {
                    (
                        state.round(),
                        state.locked_digest().and_then(duties),
                        state.prevote().and_then(BftValue::digest).and_then(duties),
                        state
                            .precommit()
                            .and_then(BftValue::digest)
                            .and_then(duties),
                        state.finality_ready_digest().is_some(),
                    )
                })
                .collect::<Vec<_>>();
            eprintln!(
                "membership candidate sizes / voting state at node {}: {candidates:?} / {voting:?}",
                index + 1
            );
            let business = tasks
                .iter()
                .map(|task| {
                    let id = task.task_id();
                    let plan = snapshot.prepared_tasks.get(&id);
                    let vote = snapshot.bft_local_states.get(&(
                        ValidatorId::new(index as u64 + 1),
                        ConsensusScope::PreparedTask(id.clone()),
                    ));
                    (
                        snapshot.state.task_succeeded(id.clone()),
                        plan.map(|plan| {
                            (
                                plan.commit_authorized,
                                plan.phase,
                                plan.conflict_abort,
                                plan.finality_votes.is_some(),
                            )
                        }),
                        vote.map(|vote| {
                            (
                                vote.round(),
                                vote.locked_digest().is_some(),
                                vote.finality_ready_digest().is_some(),
                            )
                        }),
                    )
                })
                .collect::<Vec<_>>();
            eprintln!(
                "business outcomes / ownership, phase, abort, proof / round, lock, ready at node {}: {business:?}",
                index + 1
            );
            let mut errors = std::collections::BTreeMap::new();
            for event in node.drain_bft_consensus_events().unwrap() {
                match event {
                    BftConsensusEvent::Rejected { error, .. } => {
                        *errors.entry(format!("{error:?}")).or_insert(0_usize) += 1;
                    }
                    BftConsensusEvent::ConnectionFailed { error, .. } => {
                        *errors
                            .entry(format!("connection: {error:?}"))
                            .or_insert(0_usize) += 1;
                    }
                    BftConsensusEvent::SendFailed { failures, .. } => {
                        for failure in failures {
                            *errors
                                .entry(format!("send: {:?}", failure.error))
                                .or_insert(0_usize) += 1;
                        }
                    }
                    _ => {}
                }
            }
            eprintln!(
                "membership rejection counts at node {}: {errors:?}",
                index + 1
            );
        }
    }
    assert!(
        completed.is_ok(),
        "automatic authenticated collection and membership BFT did not converge"
    );
    let mut chosen = None;
    for (index, (store, base)) in stores.iter().enumerate() {
        let cold = StateStore::new(base).load().unwrap().unwrap();
        if index >= online {
            assert_eq!(cold.validator_set.version(), 1);
            assert!(cold.prepared_tasks[&tasks[index].task_id()].commit_authorized);
            assert!(cold.state.business.accounts.is_empty());
            assert!(cold.validator_vote_locks.is_empty());
            continue;
        }
        assert_eq!(cold.validator_set.version(), 2);
        if !running {
            assert!(cold.state.business.accounts.is_empty());
            assert_eq!(
                cold.state
                    .protocol
                    .task_handoff
                    .as_ref()
                    .unwrap()
                    .plans
                    .len(),
                online
            );
        } else {
            assert_eq!(cold.state.business.accounts.len(), 4);
        }
        let digest = cold.validator_transition_proofs[&1]
            .source()
            .task_handoff_digest();
        if let Some(expected) = chosen {
            assert_eq!(digest, expected);
        } else {
            chosen = Some(digest);
        }
        for (owner, task) in tasks.iter().take(online).enumerate() {
            if running {
                assert_eq!(cold.state.task_succeeded(task.task_id()), Some(true));
                assert_eq!(
                    cold.state.protocol.task_bindings[&task.task_id()].request_digest,
                    task.request_digest()
                );
                continue;
            }
            assert_eq!(
                cold.prepared_tasks[&task.task_id()].commit_authorized,
                owner == index
            );
            assert!(
                cold.state
                    .protocol
                    .task_handoff
                    .as_ref()
                    .unwrap()
                    .task_context(&task.task_id())
                    .is_some()
            );
            assert!(
                !cold
                    .validator_vote_locks
                    .keys()
                    .any(|(_, scope)| *scope == ConsensusScope::PreparedTask(task.task_id()))
            );
        }
        assert_eq!(store.load().unwrap().unwrap().generation, cold.generation);
    }
    for worker in &workers {
        worker.abort();
    }
    for worker in workers {
        let _ = worker.await;
    }
    drop(nodes);
    for (store, base) in stores {
        store.remove_files().unwrap();
        let _ = std::fs::remove_file(base.with_extension("lock"));
    }
}
