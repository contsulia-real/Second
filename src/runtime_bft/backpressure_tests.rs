//! Bounded queues must preserve authenticated connections during a burst.
use super::tests::{key, temp_store, validator_set};
use super::*;
use crate::{
    AuthorizerSet, BftTimeoutConfig, ConsensusScope, FinalityStatement, SecondState, TaskId,
    ValidatorVote,
};
use std::sync::atomic::AtomicUsize;
use std::time::Duration;

#[tokio::test]
async fn inbound_authority_refresh_wait_keeps_the_network_executor_schedulable() {
    let validators = validator_set(1);
    let (store, base) = temp_store();
    store
        .initialize(&SecondState::genesis([], 1), &validators)
        .unwrap();
    let runtime = ValidatorBftRuntime::new(
        ValidatorRuntimeKeys::new(ValidatorId::new(2), key(6), key(7)),
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
        validators.clone(),
        std::iter::empty(),
    )
    .unwrap();
    let held = Arc::new(AtomicBool::new(false));
    let writer_held = Arc::clone(&held);
    let blocked_runtime = runtime.clone();
    let (locked, ready) = tokio::sync::oneshot::channel();
    let writer = std::thread::spawn(move || {
        let _guard = blocked_runtime.inner.authority_refresh.lock().unwrap();
        writer_held.store(true, Ordering::SeqCst);
        locked.send(()).unwrap();
        std::thread::sleep(Duration::from_secs(2));
        writer_held.store(false, Ordering::SeqCst);
    });
    ready.await.unwrap();
    let identity = QuicTransportIdentity::generate().unwrap();
    let server = crate::QuicServer::bind("127.0.0.1:0".parse().unwrap(), &identity).unwrap();
    let client = QuicClient::new(
        "127.0.0.1:0".parse().unwrap(),
        identity.certificate_der(),
        QuicTransportIdentity::generate().unwrap(),
    )
    .unwrap();
    let address = server.local_addr().unwrap();
    let receiver = runtime.clone();
    let (entered, accepted) = tokio::sync::oneshot::channel();
    let serving = tokio::spawn(async move {
        let peer = server.accept().await.unwrap();
        let first = peer.accept_request().await.unwrap().unwrap();
        entered.send(()).unwrap();
        receiver.serve_inbound(&peer, first).await
    });
    let peer = client.connect(address).await.unwrap();
    let authenticating = tokio::spawn(async move {
        crate::authenticate_validator_bft_peer(peer, ValidatorId::new(1), &key(3), &validators)
            .await
    });
    accepted.await.unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    let progressed_while_locked = held.load(Ordering::SeqCst);
    writer.join().unwrap();
    let authenticated = authenticating.await.unwrap().unwrap();
    authenticated.close();
    serving.await.unwrap().unwrap();
    assert!(
        progressed_while_locked,
        "inbound authority refresh blocked unrelated network scheduling"
    );
    drop(runtime);
    store.remove_files().unwrap();
    let _ = std::fs::remove_file(base.with_extension("lock"));
}

#[test]
fn continuously_refilled_intake_keeps_each_consensus_batch_bounded() {
    let (store, base) = temp_store();
    let validators = validator_set(1);
    store
        .initialize(&SecondState::genesis([], 1), &validators)
        .unwrap();
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
        validators,
        std::iter::empty(),
    )
    .unwrap();
    let envelope = InboundBftMessage {
        validator_id: ValidatorId::new(1),
        message: BftNetworkMessage::PreparedTaskRequest {
            validator_set_version: 1,
            scope: ConsensusScope::PreparedTask(TaskId::parse("continuous-intake").unwrap()),
            expected_plan_digest: [7; 32],
            offset: 0,
        },
    };
    for _ in 0..MAX_BFT_RUNTIME_INBOUND_QUEUE {
        runtime
            .inner
            .inbound_sender
            .try_send(envelope.clone())
            .unwrap();
    }
    let sender = runtime.inner.inbound_sender.clone();
    let producer = std::thread::spawn(move || {
        for _ in 0..MAX_BFT_RUNTIME_INBOUND_QUEUE * 32 {
            sender.blocking_send(envelope.clone()).unwrap();
        }
    });
    let mut largest = 0;
    let mut received = 0;
    while received < MAX_BFT_RUNTIME_INBOUND_QUEUE * 33 {
        let batch = runtime.drain_inbound();
        largest = largest.max(batch.len());
        received += batch.len();
        if batch.is_empty() {
            std::thread::yield_now();
        }
    }
    producer.join().unwrap();
    assert!(
        largest <= MAX_BFT_RUNTIME_INBOUND_QUEUE,
        "one intake expanded to {largest} messages despite the bounded queue"
    );
    assert_eq!(received, MAX_BFT_RUNTIME_INBOUND_QUEUE * 33);
    drop(runtime);
    store.remove_files().unwrap();
    let _ = std::fs::remove_file(base.with_extension("lock"));
}

#[tokio::test]
async fn full_normal_and_finality_queues_preserve_peer_and_resume_without_redial() {
    let validators = validator_set(1);
    let (store, base) = temp_store();
    store
        .initialize(&SecondState::genesis([], 1), &validators)
        .unwrap();
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
        validators.clone(),
        std::iter::empty(),
    )
    .unwrap();
    let identity = QuicTransportIdentity::generate().unwrap();
    let server = crate::QuicServer::bind("127.0.0.1:0".parse().unwrap(), &identity).unwrap();
    let client = QuicClient::new(
        "0.0.0.0:0".parse().unwrap(),
        identity.certificate_der(),
        QuicTransportIdentity::generate().unwrap(),
    )
    .unwrap();
    let address = server.local_addr().unwrap();
    let (received, mut arrivals) = mpsc::channel(4);
    let receiving_authority = Arc::new(std::sync::RwLock::new(
        ValidatorBftAuthority::new(validators.clone(), std::iter::empty()).unwrap(),
    ));
    let server_authority = Arc::clone(&receiving_authority);
    let serving = tokio::spawn(async move {
        let peer = server.accept().await.unwrap();
        let first = peer.accept_request().await.unwrap().unwrap();
        crate::network::serve_validator_bft_connection_from_request(
            &peer,
            first,
            ValidatorId::new(2),
            &key(6),
            &server_authority,
            |_, message| {
                received.try_send(message).unwrap();
                std::future::ready(Ok(()))
            },
        )
        .await
    });
    let transport_peer = client.connect(address).await.unwrap();
    let peer = authenticate_validator_bft_peer_with_authority(
        transport_peer.clone(),
        ValidatorId::new(1),
        &key(3),
        &runtime.inner.authority,
    )
    .await
    .unwrap();
    // Hold both real bounded queue receivers to model a paused send worker.
    // The underlying peer is still the same authenticated QUIC connection.
    let (sender, mut normal) = mpsc::channel(1);
    let (finality_sender, mut finality) = mpsc::channel(1);
    let alive = Arc::new(AtomicBool::new(true));
    let active = Arc::new(AtomicUsize::new(0));
    runtime.inner.outbound.lock().unwrap().insert(
        ValidatorId::new(2),
        ManagedValidatorBftPeer {
            peer: peer.clone(),
            sender,
            finality_sender,
            alive: Arc::clone(&alive),
            _client: client,
            _permit: ActiveConnectionPermit::try_acquire(&active).unwrap(),
        },
    );
    let scope = ConsensusScope::PreparedTask(TaskId::parse("queue-burst").unwrap());
    let statement = FinalityStatement::new(1, 1, [7; 32]);
    let messages = [
        BftNetworkMessage::PreparedTaskRequest {
            validator_set_version: 1,
            scope: scope.clone(),
            expected_plan_digest: [7; 32],
            offset: 0,
        },
        BftNetworkMessage::FinalityVote {
            scope,
            statement,
            vote: ValidatorVote::sign_unchecked(&statement, ValidatorId::new(1), &key(4)),
        },
    ];
    for (index, message) in messages.iter().enumerate() {
        runtime.send_direct(ValidatorId::new(2), message).unwrap();
        assert!(
            matches!(runtime.send_direct(ValidatorId::new(2), message), Err(NetworkError::Transport(detail)) if detail == "validator BFT send queue is full")
        );
        assert_eq!(
            runtime.connected_validator_ids(),
            vec![ValidatorId::new(2)],
            "temporary backpressure must not disconnect a live peer"
        );
        assert!(alive.load(Ordering::Acquire));
        let failures = runtime.broadcast(message);
        assert_eq!(failures.len(), 1);
        assert_eq!(runtime.connected_validator_ids(), vec![ValidatorId::new(2)]);
        let receiver = if index == 0 {
            &mut normal
        } else {
            &mut finality
        };
        let queued = receiver.try_recv().unwrap();
        assert_eq!(&queued, message);
        peer.send(&queued).await.unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), arrivals.recv())
                .await
                .unwrap()
                .unwrap(),
            *message
        );
        assert!(runtime.broadcast(message).is_empty());
        let queued = receiver.try_recv().unwrap();
        peer.send(&queued).await.unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), arrivals.recv())
                .await
                .unwrap()
                .unwrap(),
            *message
        );
    }
    // Both queues can contain valid old-frontier messages when membership
    // changes before the real send worker gets to them.
    let old_scope = ConsensusScope::CurrencyAllocation {
        validator_set_version: 1,
        start: 1,
    };
    // A valid control vote may already be on the wire when the receiver
    // switches committees. It must not be delivered or tear down the peer.
    *receiving_authority.write().unwrap() =
        ValidatorBftAuthority::new(validator_set(2), [validator_set(1)]).unwrap();
    peer.send(&BftNetworkMessage::FinalityVote {
        scope: old_scope.clone(),
        statement,
        vote: ValidatorVote::sign_unchecked(&statement, ValidatorId::new(1), &key(4)),
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !serving.is_finished(),
        "a valid obsolete control message killed the receiving connection"
    );
    assert!(matches!(
        arrivals.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    runtime
        .send_direct(
            ValidatorId::new(2),
            &BftNetworkMessage::PreparedTaskSourceUnavailable {
                validator_set_version: 1,
                scope: old_scope.clone(),
                expected_plan_digest: [7; 32],
            },
        )
        .unwrap();
    runtime
        .send_direct(
            ValidatorId::new(2),
            &BftNetworkMessage::FinalityVote {
                scope: old_scope,
                statement,
                vote: ValidatorVote::sign_unchecked(&statement, ValidatorId::new(1), &key(4)),
            },
        )
        .unwrap();
    *runtime.inner.authority.write().unwrap() =
        ValidatorBftAuthority::new(validator_set(2), [validator_set(1)]).unwrap();
    let worker = tokio::spawn(run_validator_bft_sender(
        peer.clone(),
        ValidatorId::new(2),
        normal,
        finality,
        Arc::clone(&alive),
        Arc::downgrade(&runtime.inner),
    ));
    let mut denied = 0;
    tokio::time::timeout(Duration::from_secs(3), async {
        while denied < 2 {
            for event in runtime.consensus().drain_events() {
                if let crate::BftConsensusEvent::SendFailed { failures, .. } = event {
                    denied += failures
                        .iter()
                        .filter(|failure| failure.error == NetworkError::BftUnauthorized)
                        .count();
                }
            }
            assert!(
                alive.load(Ordering::Acquire),
                "a locally rejected stale message closed the authenticated peer"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(active.load(Ordering::Acquire), 1);
    runtime
        .send_direct(ValidatorId::new(2), &messages[0])
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), arrivals.recv())
            .await
            .unwrap()
            .unwrap(),
        messages[0]
    );
    assert_eq!(runtime.connected_validator_ids(), vec![ValidatorId::new(2)]);
    // The obsolete-message path still verifies the original committee's
    // signature; a forged stale vote must fail closed on this same connection.
    transport_peer
        .send_one_way(&crate::NetworkMessage::BftMessage {
            bytes: crate::network::encode_bft_network_message(&BftNetworkMessage::FinalityVote {
                scope: ConsensusScope::CurrencyAllocation {
                    validator_set_version: 1,
                    start: 1,
                },
                statement,
                vote: ValidatorVote::sign_unchecked(&statement, ValidatorId::new(1), &key(7)),
            })
            .unwrap(),
        })
        .await
        .unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(3), serving)
            .await
            .unwrap()
            .unwrap(),
        Err(NetworkError::ConsensusFinality(_))
    ));
    // A genuinely closed worker must still retire the connection and its permit.
    worker.abort();
    let _ = worker.await;
    assert!(
        matches!(runtime.send_direct(ValidatorId::new(2), &messages[0]), Err(NetworkError::Transport(detail)) if detail == "validator BFT send worker is closed")
    );
    assert!(runtime.connected_validator_ids().is_empty());
    assert!(!alive.load(Ordering::Acquire));
    assert_eq!(active.load(Ordering::Acquire), 0);
    drop(runtime);
    store.remove_files().unwrap();
    let _ = std::fs::remove_file(base.with_extension("lock"));
}

#[tokio::test]
async fn full_inbound_queue_waits_and_resumes_on_the_same_authenticated_peer() {
    let validators = validator_set(1);
    let (store, base) = temp_store();
    store
        .initialize(&SecondState::genesis([], 1), &validators)
        .unwrap();
    let runtime = ValidatorBftRuntime::new(
        ValidatorRuntimeKeys::new(ValidatorId::new(2), key(6), key(7)),
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
        validators.clone(),
        std::iter::empty(),
    )
    .unwrap();
    let scope = ConsensusScope::PreparedTask(TaskId::parse("inbound-queue-burst").unwrap());
    let message = |offset| BftNetworkMessage::PreparedTaskRequest {
        validator_set_version: 1,
        scope: scope.clone(),
        expected_plan_digest: [8; 32],
        offset,
    };
    for _ in 0..MAX_BFT_RUNTIME_INBOUND_QUEUE {
        runtime
            .inner
            .inbound_sender
            .try_send(InboundBftMessage {
                validator_id: ValidatorId::new(1),
                message: message(0),
            })
            .unwrap();
    }
    assert_eq!(runtime.inner.inbound_sender.capacity(), 0);
    let identity = QuicTransportIdentity::generate().unwrap();
    let server = crate::QuicServer::bind("127.0.0.1:0".parse().unwrap(), &identity).unwrap();
    let client = QuicClient::new(
        "0.0.0.0:0".parse().unwrap(),
        identity.certificate_der(),
        QuicTransportIdentity::generate().unwrap(),
    )
    .unwrap();
    let address = server.local_addr().unwrap();
    let active = Arc::new(AtomicUsize::new(0));
    let permit = ActiveConnectionPermit::try_acquire(&active).unwrap();
    let receiver = runtime.clone();
    let mut serving = tokio::spawn(async move {
        let _permit = permit;
        let peer = server.accept().await.unwrap();
        let first = peer.accept_request().await.unwrap().unwrap();
        let result = receiver.serve_inbound(&peer, first).await;
        if result.is_err() {
            peer.close();
        }
        result
    });
    let peer = crate::authenticate_validator_bft_peer(
        client.connect(address).await.unwrap(),
        ValidatorId::new(1),
        &key(3),
        &validators,
    )
    .await
    .unwrap();
    peer.send(&message(1))
        .await
        .expect("queue saturation must preserve the connection");
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut serving)
            .await
            .is_err(),
        "a full intake must wait for capacity instead of terminating its authenticated session"
    );
    assert_eq!(active.load(Ordering::Acquire), 1);
    // Resume the real consensus receiver and observe the retained message.
    for offset in [1, 2] {
        if offset == 2 {
            peer.send(&message(offset)).await.unwrap();
        }
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let received = runtime.drain_inbound();
                assert!(
                    received
                        .iter()
                        .all(|envelope| envelope.validator_id == ValidatorId::new(1))
                );
                if received
                    .iter()
                    .any(|envelope| envelope.message == message(offset))
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("same authenticated peer must resume delivery after intake drains");
        assert!(!serving.is_finished());
        assert_eq!(active.load(Ordering::Acquire), 1);
    }
    runtime.inner.inbound_receiver.lock().unwrap().close();
    let _ = peer.send(&message(3)).await;
    let closed = tokio::time::timeout(Duration::from_secs(3), serving)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(closed,
        Err(ValidatorBftRuntimeError::Network(NetworkError::Transport(detail)))
            if detail == "validator BFT inbound queue is closed"
    ));
    assert_eq!(active.load(Ordering::Acquire), 0);
    let failures = runtime.consensus().drain_events();
    assert!(
        matches!(failures.as_slice(),
            [crate::BftConsensusEvent::ConnectionFailed { node_id, error, .. }]
                if *node_id == client.node_id()
                    && matches!(error,
                        ValidatorBftRuntimeError::Network(NetworkError::Transport(detail))
                            if detail == "validator BFT inbound queue is closed")
        ),
        "inbound session failure must retain the original cause: {failures:?}"
    );
    peer.close();
    client.wait_idle().await;
    drop(runtime);
    store.remove_files().unwrap();
    let _ = std::fs::remove_file(base.with_extension("lock"));
}
