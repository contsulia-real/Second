use super::*;
use crate::network::{NetworkMessage, QuicServer, QuicTransportIdentity};
use crate::runtime_bft::tests::{key, temp_store, validator_set};
use crate::runtime_bft::{ValidatorRuntimeConfig, ValidatorRuntimeKeys};
use crate::{
    AuthorizerSet, BftTimeoutConfig, CertifiedStateRecoveryCheckpoint,
    CertifiedValidatorSetTransition, SecondState, StateRecoveryCheckpoint, StateRecoveryPayload,
    ValidatorId, ValidatorSetTransition, ValidatorSetTransitionProof, ValidatorVote,
};

#[tokio::test]
async fn membership_chase_fetches_certified_handoff_without_importing_resource_rights() {
    let set = validator_set(1);
    let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
    let (provider, provider_base) = temp_store();
    let (receiver, receiver_base) = temp_store();
    let initial = SecondState::genesis([], 1);
    for store in [&provider, &receiver] {
        store.initialize(&initial, &set).unwrap();
    }
    let omitted = crate::test_helpers::sign(
        crate::LegalTaskPayload::new(
            crate::TaskId::parse("membership-offline-local-account").unwrap(),
            1,
            None,
            vec![crate::Operation::RegisterAccount {
                account: crate::test_helpers::account(238),
            }],
        ),
        &key(9),
    )
    .unwrap()
    .verify(&authorizers)
    .unwrap();
    crate::PreparedTaskBook::new(receiver.clone())
        .unwrap()
        .prepare(&mut initial.clone(), &omitted, 1, &set)
        .unwrap();
    let task = crate::test_helpers::sign(
        crate::LegalTaskPayload::new(
            crate::TaskId::parse("membership-remote-frozen-account").unwrap(),
            1,
            None,
            vec![crate::Operation::RegisterAccount {
                account: crate::test_helpers::account(239),
            }],
        ),
        &key(9),
    )
    .unwrap()
    .verify(&authorizers)
    .unwrap();
    crate::PreparedTaskBook::new(provider.clone())
        .unwrap()
        .prepare(&mut initial.clone(), &task, 1, &set)
        .unwrap();
    let trusted = provider.load().unwrap().unwrap();
    let transition = provider
        .prepare_validator_set_transition(
            ValidatorSetTransition::new(
                1,
                &set,
                &trusted.validator_registry,
                validator_set(2),
                vec![],
                vec![],
                1,
            )
            .unwrap(),
        )
        .unwrap();
    let statement = transition.finality_statement();
    let certified = CertifiedValidatorSetTransition::new(
        transition,
        (2..=4)
            .map(|id| {
                ValidatorVote::sign_unchecked(
                    &statement,
                    ValidatorId::new(id),
                    &key((id * 3 + 1) as u8),
                )
            })
            .collect(),
        &set,
    )
    .unwrap();
    provider
        .activate_validator_set_transition(&certified)
        .unwrap();
    let node = std::sync::Arc::new(
        crate::NodeRuntime::load_and_bind("127.0.0.1:0".parse().unwrap(), &provider).unwrap(),
    );
    let running = node.clone();
    let worker = tokio::spawn(async move { running.run(&[]).await });
    let client = QuicClient::new(
        "127.0.0.1:0".parse().unwrap(),
        node.transport_certificate_der(),
        QuicTransportIdentity::generate().unwrap(),
    )
    .unwrap();
    for valid_authorizers in [false, true] {
        let runtime = ValidatorBftRuntime::new(
            ValidatorRuntimeKeys::new(ValidatorId::new(1), key(3), key(4)),
            ValidatorRuntimeConfig::new(
                if valid_authorizers {
                    authorizers.clone()
                } else {
                    AuthorizerSet::new(1, [key(8).verifying_key().to_bytes()]).unwrap()
                },
                BftTimeoutConfig::new(
                    Duration::from_secs(1),
                    Duration::from_secs(1),
                    Duration::from_secs(1),
                ),
                || 1,
            ),
            receiver.clone(),
            set.clone(),
            [],
        )
        .unwrap();
        let peer = client.connect(node.local_addr().unwrap()).await.unwrap();
        let before = receiver.load().unwrap().unwrap().generation;
        let result = runtime.apply_membership_chain(&peer, &client, 2).await;
        peer.close();
        let cold = crate::StateStore::new(&receiver_base)
            .load()
            .unwrap()
            .unwrap();
        if valid_authorizers {
            result.unwrap();
            assert_eq!(cold.validator_set.version(), 2);
            assert_eq!(
                cold.state.protocol.task_handoff.as_ref(),
                provider
                    .load()
                    .unwrap()
                    .unwrap()
                    .state
                    .protocol
                    .task_handoff
                    .as_ref()
            );
            assert!(!cold.prepared_tasks[&task.task_id()].has_owned_candidate());
            assert_eq!(cold.state.task_succeeded(task.task_id()), Some(false));
            assert!(!cold.state.task_cancelled(task.task_id()));
            assert_eq!(
                cold.state.bound_request_digest(task.task_id()),
                Some(task.request_digest())
            );
            assert!(!cold.state.has_account(crate::test_helpers::account(239)));
            assert!(cold.validator_vote_locks.is_empty());
            assert!(cold.bft_local_states.is_empty());
            assert!(!cold.prepared_tasks.contains_key(&omitted.task_id()));
            assert_eq!(
                cold.state.protocol.task_bindings[&omitted.task_id()]
                    .allocation_task
                    .as_ref(),
                Some(omitted.signed_task())
            );
            assert_eq!(
                cold.state.bound_request_digest(omitted.task_id()),
                Some(omitted.request_digest())
            );
            assert!(!cold.state.has_account(crate::test_helpers::account(238)));
        } else {
            assert!(result.is_err());
            assert_eq!(cold.generation, before);
            assert_eq!(cold.validator_set.version(), 1);
            assert_eq!(cold.prepared_tasks.len(), 1);
            assert!(cold.prepared_tasks[&omitted.task_id()].has_owned_candidate());
            assert!(
                cold.state.protocol.task_bindings[&omitted.task_id()]
                    .allocation_task
                    .is_none()
            );
        }
    }
    worker.abort();
    let _ = worker.await;
    client.wait_idle().await;
    drop(node);
    provider.remove_files().unwrap();
    receiver.remove_files().unwrap();
    for suffix in ["transport", "transport.lock", "peers"] {
        let _ = std::fs::remove_file(provider_base.with_extension(suffix));
    }
}

#[tokio::test]
async fn membership_proof_install_wait_does_not_block_network_scheduling() {
    let set = validator_set(1);
    let (store, base) = temp_store();
    store
        .initialize(&SecondState::genesis([], 1), &set)
        .unwrap();
    let initial = store.load().unwrap().unwrap();
    let transition = store
        .prepare_validator_set_transition(
            ValidatorSetTransition::new(
                1,
                &set,
                &initial.validator_registry,
                validator_set(2),
                vec![],
                vec![],
                1,
            )
            .unwrap(),
        )
        .unwrap();
    let statement = transition.finality_statement();
    let proof = ValidatorSetTransitionProof::from_certified(
        &CertifiedValidatorSetTransition::new(
            transition,
            (2..=4)
                .map(|id| {
                    ValidatorVote::sign_unchecked(
                        &statement,
                        ValidatorId::new(id),
                        &key((id * 3 + 1) as u8),
                    )
                })
                .collect(),
            &set,
        )
        .unwrap(),
    );
    let runtime = ValidatorBftRuntime::new(
        ValidatorRuntimeKeys::new(ValidatorId::new(1), key(3), key(4)),
        ValidatorRuntimeConfig::new(
            AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap(),
            BftTimeoutConfig::new(
                Duration::from_secs(1),
                Duration::from_secs(1),
                Duration::from_secs(1),
            ),
            || 1,
        ),
        store.clone(),
        set,
        [],
    )
    .unwrap();
    let held = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let writer_held = held.clone();
    let lock_base = base.clone();
    let (start, wait) = std::sync::mpsc::channel();
    let (locked, lock_ready) = tokio::sync::oneshot::channel();
    let writer = std::thread::spawn(move || {
        wait.recv().unwrap();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(lock_base)
            .unwrap();
        file.lock().unwrap();
        writer_held.store(true, std::sync::atomic::Ordering::SeqCst);
        locked.send(()).unwrap();
        std::thread::sleep(Duration::from_secs(2));
        writer_held.store(false, std::sync::atomic::Ordering::SeqCst);
        file.unlock().unwrap();
    });
    let identity = QuicTransportIdentity::generate().unwrap();
    let server = QuicServer::bind("127.0.0.1:0".parse().unwrap(), &identity).unwrap();
    let address = server.local_addr().unwrap();
    let client = QuicClient::new(
        "127.0.0.1:0".parse().unwrap(),
        identity.certificate_der(),
        QuicTransportIdentity::generate().unwrap(),
    )
    .unwrap();
    let (responded, response_ready) = tokio::sync::oneshot::channel();
    let serving = tokio::spawn(async move {
        let peer = server.accept().await.unwrap();
        let request = peer.accept_request().await.unwrap().unwrap();
        assert!(matches!(
            request.message(),
            NetworkMessage::GetValidatorSetTransitionProof {
                current_validator_set_version: 1
            }
        ));
        start.send(()).unwrap();
        lock_ready.await.unwrap();
        request
            .respond(&NetworkMessage::ValidatorSetTransitionProof { proof })
            .await
            .unwrap();
        responded.send(()).unwrap();
        let _ = peer.accept_request().await;
    });
    let peer = client.connect(address).await.unwrap();
    let syncing = runtime.clone();
    let sync_peer = peer.clone();
    let installing =
        tokio::spawn(async move { syncing.apply_membership_chain(&sync_peer, &client, 2).await });
    response_ready.await.unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    let progressed = held.load(std::sync::atomic::Ordering::SeqCst);
    writer.join().unwrap();
    installing.await.unwrap().unwrap();
    peer.close();
    serving.await.unwrap();
    assert_eq!(store.load().unwrap().unwrap().validator_set.version(), 2);
    assert!(
        progressed,
        "membership proof installation blocked unrelated network scheduling"
    );
    drop(runtime);
    store.remove_files().unwrap();
}

#[tokio::test]
async fn missing_forged_and_wrong_frontier_proofs_leave_recovered_signing_locked() {
    let set = validator_set(1);
    let state = SecondState::genesis([], 1);
    let (source, _) = temp_store();
    source.initialize(&state, &set).unwrap();
    let initial = source.load().unwrap().unwrap();
    let checkpoint = StateRecoveryCheckpoint::from_persisted(1, &initial).unwrap();
    let votes = (2..=4)
        .map(|id| {
            ValidatorVote::sign_unchecked(
                &checkpoint.finality_statement(),
                ValidatorId::new(id),
                &key((id * 3 + 1) as u8),
            )
        })
        .collect();
    let recovery = CertifiedStateRecoveryCheckpoint::new(checkpoint, votes, &set).unwrap();
    let (store, _) = temp_store();
    store
        .install_recovered_state(
            &StateRecoveryPayload::from_persisted(&initial).unwrap(),
            &recovery,
            &set,
        )
        .unwrap();
    source.remove_files().unwrap();
    let runtime = ValidatorBftRuntime::new(
        ValidatorRuntimeKeys::new(ValidatorId::new(1), key(3), key(4)),
        ValidatorRuntimeConfig::new(
            AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap(),
            BftTimeoutConfig::new(
                Duration::from_secs(1),
                Duration::from_secs(1),
                Duration::from_secs(1),
            ),
            || 1,
        ),
        store.clone(),
        set.clone(),
        [],
    )
    .unwrap();
    let make_proof = |frontier| {
        let transition = ValidatorSetTransition::new(
            1,
            &set,
            &initial.validator_registry,
            validator_set(2),
            vec![],
            vec![],
            frontier,
        )
        .unwrap();
        let votes = (2..=4)
            .map(|id| {
                ValidatorVote::sign_unchecked(
                    &transition.finality_statement(),
                    ValidatorId::new(id),
                    &key((id * 3 + 1) as u8),
                )
            })
            .collect();
        ValidatorSetTransitionProof::from_certified(
            &CertifiedValidatorSetTransition::new(transition, votes, &set).unwrap(),
        )
    };
    let mut forged = make_proof(1).encode_bytes().unwrap();
    *forged.last_mut().unwrap() ^= 1;
    let responses = [
        NetworkMessage::NoValidatorSetTransitionProof,
        NetworkMessage::ValidatorSetTransitionProof {
            proof: ValidatorSetTransitionProof::decode_bytes(&forged).unwrap(),
        },
        NetworkMessage::ValidatorSetTransitionProof {
            proof: make_proof(2),
        },
    ];
    let identity = QuicTransportIdentity::generate().unwrap();
    let server = QuicServer::bind("127.0.0.1:0".parse().unwrap(), &identity).unwrap();
    let address = server.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let peer = server.accept().await.unwrap();
        for response in responses {
            let request = peer.accept_request().await.unwrap().unwrap();
            assert!(matches!(
                request.message(),
                NetworkMessage::GetValidatorSetTransitionProof {
                    current_validator_set_version: 1
                }
            ));
            request.respond(&response).await.unwrap();
        }
        let _ = peer.accept_request().await;
    });
    let client = QuicClient::new(
        "127.0.0.1:0".parse().unwrap(),
        identity.certificate_der(),
        QuicTransportIdentity::generate().unwrap(),
    )
    .unwrap();
    let peer = client.connect(address).await.unwrap();
    let before = store.load().unwrap().unwrap();
    for _ in 0..3 {
        assert!(
            runtime
                .apply_membership_chain(&peer, &client, 2)
                .await
                .is_err()
        );
        let snapshot = store.load().unwrap().unwrap();
        assert_eq!(snapshot.generation, before.generation);
        assert_eq!(snapshot.validator_set, before.validator_set);
        assert!(!snapshot.validator_safety_ready);
        assert_eq!(snapshot.minimum_signing_validator_set_version, 1);
    }
    peer.close();
    task.await.unwrap();
    drop(runtime);
    store.remove_files().unwrap();
}
