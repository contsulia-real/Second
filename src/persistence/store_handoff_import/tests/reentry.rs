//! Installed members must regain signing through the existing fresh-key fence.
use super::*;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn imported_member_rotates_recovers_and_commits_business_through_running_nodes() {
    let origin = validator_set();
    let account = crate::test_helpers::account(201);
    let initial = SecondState::genesis([account], 1).with_reserve(2).unwrap();
    let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
    let mut stores = (0..5).map(|_| temp_store()).collect::<Vec<_>>();
    for (store, _) in &stores[..4] {
        store.initialize(&initial, &origin).unwrap();
    }
    let trusted = stores[0].0.load().unwrap().unwrap();
    let joining = ValidatorCredential::new(
        ValidatorId::new(5),
        key(15).verifying_key().to_bytes(),
        key(16).verifying_key().to_bytes(),
        key(17).verifying_key().to_bytes(),
    )
    .unwrap();
    let admission =
        ValidatorAdmissionRequest::sign(1, joining.clone(), &key(15), &key(16), &key(17))
            .unwrap()
            .verify()
            .unwrap();
    let next = ValidatorSet::new(
        2,
        origin
            .credentials()
            .cloned()
            .chain(std::iter::once(joining)),
    )
    .unwrap();
    let transition = stores[0]
        .0
        .prepare_validator_set_transition(
            ValidatorSetTransition::new(
                1,
                &origin,
                &trusted.validator_registry,
                next.clone(),
                vec![admission],
                vec![],
                3,
            )
            .unwrap(),
        )
        .unwrap();
    let statement = transition.finality_statement();
    let certified = CertifiedValidatorSetTransition::new(
        transition,
        (1..=3)
            .map(|id| {
                ValidatorVote::sign_unchecked(
                    &statement,
                    ValidatorId::new(id),
                    &key((id * 3 + 1) as u8),
                )
            })
            .collect(),
        &origin,
    )
    .unwrap();
    for (store, _) in &stores[..4] {
        store.activate_validator_set_transition(&certified).unwrap();
    }
    let bind = |id: u64, store: &StateStore| {
        Arc::new(
            NodeRuntime::bind_loaded(
                "127.0.0.1:0".parse().unwrap(),
                store,
                store.load().unwrap().unwrap(),
                NodeRuntimeCapabilities::default().with_validator(
                    ValidatorRuntimeKeys::new(
                        ValidatorId::new(id),
                        key((id * 3) as u8),
                        key((id * 3 + 1) as u8),
                    )
                    .with_consensus_key(key(90)),
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
    let mut nodes = (1..=4)
        .map(|id| bind(id, &stores[id as usize - 1].0))
        .collect::<Vec<_>>();
    let mut workers = Vec::new();
    let provider = nodes[0].clone();
    workers.push(tokio::spawn(async move { provider.run(&[]).await }));
    let client = QuicClient::new(
        "127.0.0.1:0".parse().unwrap(),
        nodes[0].transport_certificate_der(),
        QuicTransportIdentity::generate().unwrap(),
    )
    .unwrap();
    let peer = client
        .connect(nodes[0].local_addr().unwrap())
        .await
        .unwrap();
    let proof = client_validator_set_transition_proof(&peer, 1)
        .await
        .unwrap()
        .unwrap();
    peer.close();
    let peer = client
        .connect(nodes[0].local_addr().unwrap())
        .await
        .unwrap();
    let body =
        client_fetch_validator_handoff(&peer, ValidatorId::new(5), &key(15), &proof, &trusted)
            .await
            .unwrap();
    peer.close();
    stores[4]
        .0
        .install_validator_handoff_baseline(&proof, &body, &trusted, &authorizers)
        .unwrap();
    let installed = stores[4].0.load().unwrap().unwrap();
    assert!(installed.state.has_account(account));
    assert_eq!(installed.state.reserve_count(), 2);
    assert!(!installed.validator_safety_ready);
    let checkpoint = StateRecoveryCheckpoint::from_persisted(1, &installed).unwrap();
    assert_eq!(
        ValidatorSigner::new(ValidatorId::new(5), key(16), stores[4].0.clone())
            .sign_state_recovery_checkpoint(&checkpoint, &next),
        Err(ValidatorSigningError::LocalSafetyStateUnavailable),
    );
    nodes.push(bind(5, &stores[4].0));
    let records = nodes
        .iter()
        .map(|node| node.local_peer_record().unwrap().clone())
        .collect::<Vec<_>>();
    for node in &nodes[1..] {
        let node = node.clone();
        let records = records.clone();
        workers.push(tokio::spawn(async move { node.run(&records).await }));
    }
    for (index, node) in nodes.iter().enumerate() {
        for (other, record) in records.iter().enumerate() {
            if index != other {
                node.dial_validator_bft(record).await.unwrap();
            }
        }
    }
    let rotation = ValidatorConsensusKeyRotationRequest::sign(
        1,
        ValidatorRotationAuthority::Identity,
        ValidatorId::new(5),
        2,
        key(90).verifying_key().to_bytes(),
        &key(15),
    )
    .unwrap();
    let rotated = ValidatorSet::new(
        3,
        next.credentials().map(|credential| {
            if credential.id() == ValidatorId::new(5) {
                ValidatorCredential::new(
                    credential.id(),
                    credential.identity_public_key(),
                    key(90).verifying_key().to_bytes(),
                    credential.recovery_public_key(),
                )
                .unwrap()
            } else {
                credential.clone()
            }
        }),
    )
    .unwrap();
    let current = stores[0].0.load().unwrap().unwrap();
    nodes[0]
        .start_validator_set_transition_consensus(
            ValidatorSetTransition::new(
                1,
                &next,
                &current.validator_registry,
                rotated,
                vec![],
                vec![rotation],
                3,
            )
            .unwrap(),
        )
        .unwrap();
    wait_until("rotation", &stores, &nodes, || {
        stores
            .iter()
            .all(|(store, _)| store.load().unwrap().unwrap().validator_set.version() == 3)
    })
    .await;
    assert!(!stores[4].0.load().unwrap().unwrap().validator_safety_ready);
    assert!(
        stores[4]
            .0
            .load()
            .unwrap()
            .unwrap()
            .validator_vote_locks
            .is_empty()
    );
    let peer = client
        .connect(nodes[0].local_addr().unwrap())
        .await
        .unwrap();
    client_submit_recovery_checkpoint(&peer, ValidatorId::new(1), &key(3), 3)
        .await
        .unwrap();
    peer.close();
    wait_until("recovery", &stores, &nodes, || {
        stores[4].0.load().unwrap().unwrap().validator_safety_ready
    })
    .await;
    let recovered = StateStore::new(&stores[4].1).load().unwrap().unwrap();
    assert_eq!(recovered.minimum_signing_validator_set_version, 3);
    assert!(recovered.recovery_checkpoint_proof.is_some());
    let registered = crate::test_helpers::account(202);
    let task = crate::test_helpers::sign(
        LegalTaskPayload::new(
            TaskId::parse("imported-member-post-recovery-business").unwrap(),
            1,
            None,
            vec![Operation::RegisterAccount {
                account: registered,
            }],
        ),
        &key(9),
    )
    .unwrap();
    let task_id = task.payload().task_id();
    nodes[4].submit_legal_task(task).unwrap();
    wait_until("business", &stores, &nodes, || {
        stores
            .iter()
            .all(|(store, _)| store.load().unwrap().unwrap().state.has_account(registered))
    })
    .await;
    let completed = StateStore::new(&stores[4].1).load().unwrap().unwrap();
    assert_eq!(completed.state.task_succeeded(task_id.clone()), Some(true));
    assert!(
        completed
            .validator_vote_locks
            .contains_key(&(ValidatorId::new(5), ConsensusScope::PreparedTask(task_id),))
    );
    assert!(workers.iter().all(|worker| !worker.is_finished()));
    for worker in &workers {
        worker.abort();
    }
    for worker in workers {
        let _ = worker.await;
    }
    client.wait_idle().await;
    drop(nodes);
    for (store, base) in stores.drain(..) {
        store.remove_files().unwrap();
        for suffix in ["transport", "transport.lock", "peers"] {
            let _ = std::fs::remove_file(base.with_extension(suffix));
        }
    }
}

async fn wait_until(
    stage: &str,
    stores: &[(StateStore, std::path::PathBuf)],
    nodes: &[Arc<NodeRuntime>],
    mut condition: impl FnMut() -> bool,
) {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    if result.is_err() {
        for (index, (store, _)) in stores.iter().enumerate() {
            let snapshot = store.load_shared().unwrap().unwrap();
            let rounds = snapshot
                .bft_local_states
                .iter()
                .map(|((_, scope), state)| {
                    (scope, state.round(), state.prevote(), state.precommit())
                })
                .collect::<Vec<_>>();
            eprintln!(
                "{stage} node {}: version={} ready={} pending={} rounds={rounds:?}",
                index + 1,
                snapshot.validator_set.version(),
                snapshot.validator_safety_ready,
                snapshot.pending_governance.len()
            );
            let mut errors = std::collections::BTreeMap::new();
            for event in nodes[index].drain_bft_consensus_events().unwrap() {
                if let BftConsensusEvent::Rejected { error, .. } = event {
                    *errors.entry(format!("{error:?}")).or_insert(0_usize) += 1;
                }
            }
            eprintln!("{stage} node {} rejections: {errors:?}", index + 1);
        }
    }
    assert!(result.is_ok(), "new member {stage} did not converge");
}
