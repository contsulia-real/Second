//! Exercise the existing import fixture through its authenticated BFT transport.
use crate::prepared::tests::key;
use crate::*;
use std::time::Duration;

pub(super) async fn deliver(
    source: &StateStore,
    receiver: &NodeRuntime,
    authorizers: &AuthorizerSet,
    messages: [BftNetworkMessage; 2],
) {
    let identity = QuicTransportIdentity::generate().unwrap();
    let server = QuicServer::bind("127.0.0.1:0".parse().unwrap(), &identity).unwrap();
    let record = PeerRecord::new(
        server.node_id(),
        server.local_addr().unwrap(),
        identity.certificate_der().to_vec(),
    )
    .unwrap();
    let inbound = receiver.validator_bft.as_ref().unwrap().clone();
    let serving = tokio::spawn(async move {
        let peer = server.accept().await.unwrap();
        let request = peer.accept_request().await.unwrap().unwrap();
        inbound.serve_inbound(&peer, request).await
    });
    let sender = NodeRuntime::bind_loaded(
        "127.0.0.1:0".parse().unwrap(),
        source,
        source.load().unwrap().unwrap(),
        NodeRuntimeCapabilities::default().with_validator(
            ValidatorRuntimeKeys::new(ValidatorId::new(1), key(3), key(4)),
            ValidatorRuntimeConfig::new(
                authorizers.clone(),
                BftTimeoutConfig::new(
                    Duration::from_secs(1),
                    Duration::from_secs(1),
                    Duration::from_secs(1),
                ),
                || 3,
            ),
        ),
    )
    .unwrap();
    let reverse_identity = QuicTransportIdentity::generate().unwrap();
    let reverse_server =
        QuicServer::bind("127.0.0.1:0".parse().unwrap(), &reverse_identity).unwrap();
    let reverse_record = PeerRecord::new(
        reverse_server.node_id(),
        reverse_server.local_addr().unwrap(),
        reverse_identity.certificate_der().to_vec(),
    )
    .unwrap();
    let reverse_inbound = sender.validator_bft.as_ref().unwrap().clone();
    let reverse_serving = tokio::spawn(async move {
        let peer = reverse_server.accept().await.unwrap();
        let request = peer.accept_request().await.unwrap().unwrap();
        reverse_inbound.serve_inbound(&peer, request).await
    });
    assert_eq!(
        receiver.dial_validator_bft(&reverse_record).await.unwrap(),
        ValidatorId::new(1)
    );
    assert_eq!(
        sender.dial_validator_bft(&record).await.unwrap(),
        ValidatorId::new(5)
    );
    let outbound = sender.validator_bft.as_ref().unwrap();
    let BftNetworkMessage::FinalityCertificate { scope, certificate } = &messages[0] else {
        panic!("terminal fixture");
    };
    let ConsensusScope::PreparedTask(task_id) = scope else {
        panic!("task fixture");
    };
    let source_body = source.load_prepared_tasks().unwrap()[task_id]
        .encode_source()
        .unwrap();
    for denied in [
        BftNetworkMessage::FinalityVote {
            scope: scope.clone(),
            statement: certificate.statement(),
            vote: certificate.votes()[0].clone(),
        },
        BftNetworkMessage::PreparedTaskSourceChunk {
            validator_set_version: 1,
            scope: scope.clone(),
            expected_plan_digest: certificate.statement().subject_digest(),
            total_len: source_body.len() as u64,
            offset: 0,
            bytes: source_body,
        },
        BftNetworkMessage::FinalityCertificate {
            scope: ConsensusScope::PreparedTask(TaskId::parse("unlisted-old-terminal").unwrap()),
            certificate: certificate.clone(),
        },
    ] {
        assert!(outbound.broadcast(&denied).is_empty());
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        receiver
            .validator_bft
            .as_ref()
            .unwrap()
            .drain_inbound()
            .is_empty(),
        "handoff proof delivery must not grant old votes, private bodies, or unlisted scopes an audience"
    );
    assert!(!serving.is_finished());
    let terminal_scope = messages[1].scope().clone();
    for message in messages {
        assert!(outbound.broadcast(&message).is_empty());
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let received = receiver.validator_bft.as_ref().unwrap().drain_inbound();
                if !received.is_empty() {
                    assert_eq!(received.len(), 1);
                    assert_eq!(received[0].validator_id, ValidatorId::new(1));
                    assert_eq!(received[0].message, message);
                    assert!(
                        receiver
                            .process_prepared_task_sync(received)
                            .unwrap()
                            .is_empty(),
                        "inherited terminal certificate must be applied without old-set voting"
                    );
                    break;
                }
                assert!(
                    !serving.is_finished(),
                    "valid historical finality must preserve the authenticated session"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("new member must receive original-committee finality over the real connection");
    }
    let receiver_bft = receiver.validator_bft.as_ref().unwrap();
    let current_statement = FinalityStatement::new(1, 2, [27; 32]);
    let current_vote = BftNetworkMessage::FinalityVote {
        scope: terminal_scope,
        statement: current_statement,
        vote: ValidatorVote::sign_unchecked(&current_statement, ValidatorId::new(1), &key(4)),
    };
    assert!(outbound.broadcast(&current_vote).is_empty());
    let inbound = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let inbound = receiver_bft.drain_inbound();
            if !inbound.is_empty() {
                break inbound;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("current committee vote must arrive on the original authenticated session");
    assert_eq!(inbound.len(), 1);
    assert_eq!(inbound[0].message, current_vote);
    let relays = receiver_bft
        .consensus()
        .drive(
            receiver.process_prepared_task_sync(inbound).unwrap(),
            tokio::time::Instant::now(),
        )
        .outbound;
    assert!(
        relays
            .iter()
            .any(|message| matches!(message, BftNetworkMessage::FinalityCertificate { .. }))
    );
    for relay in relays {
        assert!(receiver_bft.broadcast(&relay).is_empty());
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        receiver_bft.connected_validator_ids(),
        vec![ValidatorId::new(1)],
        "passive historical completion must not close the current committee connection"
    );
    assert!(!reverse_serving.is_finished());
    assert!(
        outbound.drain_inbound().iter().all(|envelope| {
            matches!(
                envelope.message.scope(),
                ConsensusScope::CurrencyAllocation {
                    validator_set_version: 2,
                    ..
                }
            )
        }),
        "historical completion may resume current allocations, but must not send old-set messages"
    );
    let current_reply = BftNetworkMessage::FinalityVote {
        scope: ConsensusScope::PreparedTask(TaskId::parse("current-session-still-live").unwrap()),
        statement: current_statement,
        vote: ValidatorVote::sign_unchecked(&current_statement, ValidatorId::new(5), &key(16)),
    };
    assert!(receiver_bft.broadcast(&current_reply).is_empty());
    let reply = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let reply = outbound.drain_inbound();
            if !reply.is_empty() {
                break reply;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("current messages must still cross the same connection without redial");
    assert_eq!(reply.len(), 1);
    assert_eq!(reply[0].validator_id, ValidatorId::new(5));
    assert_eq!(reply[0].message, current_reply);
    assert!(
        outbound
            .consensus()
            .drive(vec![], tokio::time::Instant::now())
            .outbound
            .is_empty()
    );
    drop(sender);
    serving.abort();
    let _ = serving.await;
    reverse_serving.abort();
    let _ = reverse_serving.await;
    for extension in ["transport", "transport.lock", "peers"] {
        let _ = std::fs::remove_file(source.base_path().with_extension(extension));
    }
}
