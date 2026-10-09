use crate::support::{self, bind_validator_runtime, key, peer_record, temp_base, validator_set};
use second::*;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn validator_reconnect_progresses_while_public_discovery_is_stalled() {
    let validators = validator_set(1, 1..=2);
    let mut fixtures = Vec::new();
    for id in 1..=2 {
        let base = temp_base("validator-reconnect-public-stall");
        let store = StateStore::new(&base);
        store
            .initialize(&SecondState::genesis([], 1), &validators)
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
        fixtures.push((runtime, store, base));
    }
    let delayed_record = peer_record(&fixtures[1].0);
    let stalled_a = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let stalled_b = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let records = [
        stalled_a.local_addr().unwrap(),
        stalled_b.local_addr().unwrap(),
    ]
    .into_iter()
    .map(|address| {
        PeerRecord::new(
            delayed_record.node_id(),
            address,
            delayed_record.certificate_der().to_vec(),
        )
        .unwrap()
    })
    .chain(std::iter::once(delayed_record.clone()))
    .collect();
    let first = support::spawn_node_runtime_with_bootstrap(&fixtures[0].0, records);
    let initial_failure = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if fixtures[0]
                .0
                .drain_bft_consensus_events()
                .unwrap()
                .into_iter()
                .any(|event| {
                    matches!(event, BftConsensusEvent::ConnectionFailed { address, .. }
                    if address == delayed_record.address())
                })
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    // Let maintenance enter its public discovery request before the member
    // starts serving. The two earlier UDP endpoints each consume a deadline.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let second = support::spawn_node_runtime_with_bootstrap(
        &fixtures[1].0,
        vec![peer_record(&fixtures[0].0)],
    );
    let reconnected = tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            if fixtures[0]
                .0
                .connected_validator_ids()
                .contains(&ValidatorId::new(2))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    first.abort();
    second.abort();
    let _ = first.await;
    let _ = second.await;
    assert!(
        initial_failure.is_ok(),
        "initial validator authentication must time out"
    );
    assert!(
        reconnected.is_ok(),
        "public discovery blocked validator reconnection; fixtures={:?}",
        fixtures.iter().map(|(_, _, base)| base).collect::<Vec<_>>()
    );
    for (node, store, base) in fixtures {
        drop(node);
        support::cleanup_node_runtime(store, base);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stalled_first_validator_dial_does_not_keep_recovery_submission_busy() {
    let validators = validator_set(1, 1..=4);
    let mut fixtures = Vec::new();
    for id in 1..=4 {
        let base = temp_base(&format!("checkpoint-connectivity-{id}"));
        let store = StateStore::new(&base);
        store
            .initialize(&SecondState::genesis([], 1), &validators)
            .unwrap();
        let node = Arc::new(bind_validator_runtime(
            &store,
            ValidatorRuntimeKeys::new(
                ValidatorId::new(id),
                key((id * 3) as u8),
                key((id * 3 + 1) as u8),
            ),
            support::default_validator_runtime_config(),
        ));
        fixtures.push((node, store, base));
    }
    let mut records = fixtures
        .iter()
        .map(|(node, _, _)| peer_record(node))
        .collect::<Vec<_>>();
    // Hold an unresponsive UDP endpoint so the first dial consumes its real
    // transport deadline rather than failing immediately with a refused port.
    let stalled = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    records[1] = PeerRecord::new(
        records[1].node_id(),
        stalled.local_addr().unwrap(),
        records[1].certificate_der().to_vec(),
    )
    .unwrap();
    let mut workers = vec![support::spawn_node_runtime_with_bootstrap(
        &fixtures[0].0,
        records[1..].to_vec(),
    )];
    let client = QuicClient::new(
        "127.0.0.1:0".parse().unwrap(),
        records[0].certificate_der(),
        QuicTransportIdentity::generate().unwrap(),
    )
    .unwrap();
    let peer = client.connect(records[0].address()).await.unwrap();
    let unauthorized =
        client_submit_public_checkpoint(&peer, ValidatorId::new(1), &key(4), 1).await;
    peer.close();
    let peer = client.connect(records[0].address()).await.unwrap();
    let unavailable =
        client_submit_recovery_checkpoint(&peer, ValidatorId::new(1), &key(3), 1).await;
    peer.close();
    for index in [2, 3] {
        workers.push(support::spawn_node_runtime_with_bootstrap(
            &fixtures[index].0,
            vec![records[0].clone()],
        ));
    }
    let submission = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let peer = client.connect(records[0].address()).await.unwrap();
            let result =
                client_submit_recovery_checkpoint(&peer, ValidatorId::new(1), &key(3), 1).await;
            peer.close();
            match result {
                Ok(accepted) => break accepted,
                Err(NetworkError::GovernanceRejected(GovernanceRejection::Busy)) => {
                    tokio::time::sleep(Duration::from_millis(20)).await
                }
                other => panic!("recovery submission: {other:?}"),
            }
        }
    })
    .await;
    let connected = fixtures[0].0.connected_validator_ids();
    let certified = if submission.is_ok() {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if [0, 2, 3].into_iter().all(|index| {
                    fixtures[index]
                        .1
                        .load()
                        .unwrap()
                        .unwrap()
                        .recovery_checkpoint_proof
                        .is_some()
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .is_ok()
    } else {
        false
    };
    let failure = tokio::time::timeout(Duration::from_secs(10), async {
        'found: loop {
            for event in fixtures[0].0.drain_bft_consensus_events().unwrap() {
                if let BftConsensusEvent::ConnectionFailed {
                    node_id,
                    address,
                    elapsed,
                    error,
                } = event
                    && node_id == records[1].node_id()
                    && address == stalled.local_addr().unwrap()
                {
                    break 'found (elapsed, error);
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    for worker in &workers {
        worker.abort();
    }
    for worker in workers {
        let _ = worker.await;
    }
    assert!(
        matches!(
            unauthorized,
            Err(NetworkError::GovernanceRejected(
                GovernanceRejection::Unauthorized
            ))
        ),
        "identity rejection was hidden by availability: {unauthorized:?}"
    );
    assert!(
        matches!(
            unavailable,
            Err(NetworkError::GovernanceRejected(GovernanceRejection::Busy))
        ),
        "an authorized request without connected quorum must remain Busy: {unavailable:?}"
    );
    assert!(
        submission.is_ok(),
        "healthy quorum remained Busy behind a stalled dial: {connected:?}; fixtures={:?}",
        fixtures.iter().map(|(_, _, base)| base).collect::<Vec<_>>()
    );
    assert!(
        certified,
        "accepted recovery request did not obtain its actual quorum"
    );
    assert!(!connected.contains(&ValidatorId::new(2)));
    let (elapsed, error) = failure.expect("failed offline dial must retain its endpoint and error");
    assert!(
        elapsed >= Duration::from_secs(3),
        "stalled dial unexpectedly failed immediately: {elapsed:?}"
    );
    assert!(
        matches!(
            error,
            ValidatorBftRuntimeError::Network(NetworkError::Transport(_))
        ),
        "unexpected dial failure: {error:?}"
    );
    for (node, store, base) in fixtures {
        drop(node);
        support::cleanup_node_runtime(store, base);
    }
}
