use super::*;

#[tokio::test]
async fn peer_cache_wait_does_not_block_running_node_connections() {
    use crate::prepared::tests::{key, temp_store, validator_set};
    use crate::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    let (store, base) = temp_store();
    store
        .initialize(&SecondState::genesis([], 1), &validator_set())
        .unwrap();
    let node = Arc::new(
        NodeRuntime::bind_loaded(
            "127.0.0.1:0".parse().unwrap(),
            &store,
            store.load().unwrap().unwrap(),
            NodeRuntimeCapabilities::default().with_validator(
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
            ),
        )
        .unwrap(),
    );
    let running = node.clone();
    let worker = tokio::spawn(async move { running.run(&[]).await });
    let client = QuicClient::new(
        "127.0.0.1:0".parse().unwrap(),
        node.transport_certificate_der(),
        QuicTransportIdentity::generate().unwrap(),
    )
    .unwrap();
    let peer = client.connect(node.local_addr().unwrap()).await.unwrap();
    client_ping(&peer, 101).await.unwrap();
    let cache = node.peer_store.clone();
    let held = Arc::new(AtomicBool::new(false));
    let holding = held.clone();
    let (locked, ready) = tokio::sync::oneshot::channel();
    let writer = std::thread::spawn(move || {
        let guard = cache.records.lock().unwrap();
        holding.store(true, Ordering::SeqCst);
        locked.send(()).unwrap();
        std::thread::sleep(Duration::from_secs(2));
        holding.store(false, Ordering::SeqCst);
        drop(guard);
    });
    ready.await.unwrap();
    let bootstrap_node = node.clone();
    let bootstrap = tokio::spawn(async move { bootstrap_node.bootstrap(&[], 1).await.unwrap() });
    let validator_node = node.clone();
    let validators = tokio::spawn(async move {
        validator_node
            .maintain_validator_bft_peers(&[])
            .await
            .unwrap();
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(client_ping(&peer, 102).await.unwrap(), node.node_id());
    let responded_while_locked = held.load(Ordering::SeqCst);
    writer.join().unwrap();
    bootstrap.await.unwrap();
    validators.await.unwrap();
    peer.close();
    worker.abort();
    let _ = worker.await;
    client.wait_idle().await;
    drop(node);
    assert!(
        responded_while_locked,
        "peer cache wait blocked another connection until the writer released it"
    );
    store.remove_files().unwrap();
    for suffix in [".transport", ".transport.lock", ".peers"] {
        let _ = fs::remove_file(crate::persistence::slot::append_suffix(&base, suffix));
    }
}

fn record(id: u16) -> PeerRecord {
    let mut node = [0; 32];
    node[..2].copy_from_slice(&id.to_be_bytes());
    PeerRecord::new(
        NodeId::from_bytes(node),
        "127.0.0.1:9000".parse().unwrap(),
        vec![1],
    )
    .unwrap()
}

#[test]
fn validator_candidates_survive_public_churn_restart_and_authority_retirement() {
    let path = std::env::temp_dir().join(format!(
        "second-peer-priority-{}-{}.peers",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let cache = PeerStore::load(path.clone()).unwrap();
    let allowed = (1..=40).map(ValidatorId::new).collect::<Vec<_>>();
    for id in 1..=40 {
        cache
            .record_validator_authenticated(&record(id), ValidatorId::new(u64::from(id)))
            .unwrap();
    }
    for id in 41..=200 {
        cache.record_authenticated(&record(id)).unwrap();
    }
    drop(cache);
    let cache = PeerStore::load(path.clone()).unwrap();
    let candidates = cache
        .validator_candidates(&allowed, NodeId::from_bytes([255; 32]))
        .unwrap();
    assert_eq!(candidates.len(), usize::from(MAX_LOCAL_PEER_CANDIDATES));
    assert!(
        candidates[..40]
            .iter()
            .all(|peer| allowed.iter().any(|id| peer == &record(id.value() as u16)))
    );
    cache
        .validator_candidates(&[], NodeId::from_bytes([255; 32]))
        .unwrap();
    for id in 201..=340 {
        cache.record_authenticated(&record(id)).unwrap();
    }
    assert!(
        cache
            .recent(MAX_LOCAL_PEER_CANDIDATES, &[])
            .iter()
            .all(|peer| !allowed.iter().any(|id| peer == &record(id.value() as u16)))
    );
    fs::write(&path, vec![0; codec::MAX_STORE_SIZE + 1]).unwrap();
    assert!(
        PeerStore::load(path.clone())
            .unwrap()
            .recent(MAX_LOCAL_PEER_CANDIDATES, &[])
            .is_empty()
    );
    fs::remove_file(path).unwrap();
}

#[test]
fn one_validator_cannot_pin_multiple_transport_identities() {
    let cache = PeerStore {
        path: Arc::new(
            std::env::temp_dir().join(format!("second-peer-identity-{}.peers", std::process::id())),
        ),
        records: Arc::new(Mutex::new(Vec::new())),
    };
    for id in 1..=150 {
        cache
            .record_validator_authenticated(&record(id), ValidatorId::new(1))
            .unwrap();
    }
    assert_eq!(
        cache.recent(MAX_LOCAL_PEER_CANDIDATES, &[]),
        vec![record(150)]
    );
    let duplicate = vec![
        CachedPeer {
            record: record(1),
            validator: Some(ValidatorId::new(1)),
        },
        CachedPeer {
            record: record(2),
            validator: Some(ValidatorId::new(1)),
        },
    ];
    assert!(codec::decode(&codec::encode(&duplicate).unwrap()).is_err());
    fs::remove_file(&*cache.path).unwrap();
}
