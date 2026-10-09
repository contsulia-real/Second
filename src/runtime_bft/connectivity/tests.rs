//! Connected validator endpoints must already be recoverable from the peer cache.
use crate::prepared::tests::{key, temp_store, validator_set};
use crate::*;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn cache_write_failure_cannot_publish_an_authenticated_validator_connection() {
    let mut fixtures = Vec::new();
    for id in [1, 2] {
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
                    ValidatorRuntimeKeys::new(
                        ValidatorId::new(id),
                        key((id * 3) as u8),
                        key((id * 3 + 1) as u8),
                    ),
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
        fixtures.push((node, store, base));
    }
    let blocked_staging = crate::persistence::slot::append_suffix(&fixtures[0].2, ".peers.new");
    std::fs::create_dir(&blocked_staging).unwrap();
    let remote = fixtures[1].0.clone();
    let listener = tokio::spawn(async move { remote.run(&[]).await });
    let result = fixtures[0]
        .0
        .dial_validator_bft(fixtures[1].0.local_peer_record().unwrap())
        .await;
    assert!(
        result.is_err(),
        "cache persistence failure must reach the caller"
    );
    assert!(
        fixtures[0].0.connected_validator_ids().is_empty(),
        "a live connection was published before its authenticated endpoint was persisted"
    );
    std::fs::remove_dir(&blocked_staging).unwrap();
    // A subsequent real handshake must persist and publish exactly this member.
    assert_eq!(
        fixtures[0]
            .0
            .dial_validator_bft(fixtures[1].0.local_peer_record().unwrap())
            .await
            .unwrap(),
        ValidatorId::new(2)
    );
    assert_eq!(
        fixtures[0].0.connected_validator_ids(),
        [ValidatorId::new(2)]
    );
    let cold_cache = crate::network::PeerStore::load(crate::persistence::slot::append_suffix(
        &fixtures[0].2,
        ".peers",
    ))
    .unwrap();
    assert_eq!(
        cold_cache
            .validator_candidates(&[ValidatorId::new(2)], fixtures[0].0.node_id())
            .unwrap(),
        [fixtures[1].0.local_peer_record().unwrap().clone()]
    );
    listener.abort();
    let _ = listener.await;
    for (node, store, base) in fixtures {
        drop(node);
        store.remove_files().unwrap();
        for suffix in [".lock", ".transport", ".transport.lock", ".peers"] {
            let _ = std::fs::remove_file(crate::persistence::slot::append_suffix(&base, suffix));
        }
    }
}
