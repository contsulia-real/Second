//! The production listener must remain schedulable during a consensus store wait.
use super::*;
use crate::prepared::tests::{key, temp_store, validator_set};
use crate::*;
use std::sync::atomic::AtomicBool;

#[tokio::test]
async fn public_storage_queries_do_not_block_ping_on_another_connection() {
    for (proof_only, cancelled) in [(true, false), (false, false), (true, true)] {
        let (store, base) = temp_store();
        store
            .initialize(&SecondState::genesis([], 1), &validator_set())
            .unwrap();
        let runtime = Arc::new(
            NodeRuntime::bind_loaded(
                "127.0.0.1:0".parse().unwrap(),
                &store,
                store.load().unwrap().unwrap(),
                NodeRuntimeCapabilities::default(),
            )
            .unwrap(),
        );
        let running = runtime.clone();
        let worker = tokio::spawn(async move { running.run(&[]).await });
        let connect = || {
            QuicClient::new(
                "127.0.0.1:0".parse().unwrap(),
                runtime.transport_certificate_der(),
                QuicTransportIdentity::generate().unwrap(),
            )
            .unwrap()
        };
        let query_client = connect();
        let ping_client = connect();
        let query_peer = query_client
            .connect(runtime.local_addr().unwrap())
            .await
            .unwrap();
        let ping_peer = ping_client
            .connect(runtime.local_addr().unwrap())
            .await
            .unwrap();
        client_ping(&ping_peer, 81).await.unwrap();
        if !proof_only {
            client_ping(&query_peer, 82).await.unwrap();
        }
        let held = Arc::new(AtomicBool::new(false));
        let writer_held = held.clone();
        let lock_base = base.clone();
        let (locked, ready) = tokio::sync::oneshot::channel();
        let writer = std::thread::spawn(move || {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(lock_base)
                .unwrap();
            file.lock().unwrap();
            writer_held.store(true, Ordering::SeqCst);
            locked.send(()).unwrap();
            std::thread::sleep(Duration::from_secs(if cancelled { 7 } else { 2 }));
            writer_held.store(false, Ordering::SeqCst);
            file.unlock().unwrap();
        });
        ready.await.unwrap();
        let requesting = query_peer.clone();
        let query = tokio::spawn(async move {
            if proof_only {
                let result = client_validator_set_transition_proof(&requesting, 1).await;
                if cancelled {
                    assert!(result.is_err());
                } else {
                    assert!(result.unwrap().is_none());
                }
            } else {
                assert_eq!(
                    client_public_currency_summary(&requesting)
                        .await
                        .unwrap()
                        .summary
                        .current_supply,
                    0
                );
            }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            client_ping(&ping_peer, 83).await.unwrap(),
            runtime.node_id()
        );
        let responded_while_locked = held.load(Ordering::SeqCst);
        if cancelled {
            query.await.unwrap();
            assert!(held.load(Ordering::SeqCst));
            assert_eq!(
                runtime.active_connections.load(Ordering::Acquire),
                2,
                "cancelled proof request released its connection slot while its storage worker was still blocked"
            );
            writer.join().unwrap();
        } else {
            writer.join().unwrap();
            query.await.unwrap();
        }
        query_peer.close();
        ping_peer.close();
        worker.abort();
        let _ = worker.await;
        assert!(
            responded_while_locked,
            "public storage query blocked another connection; proof_only={proof_only}"
        );
        drop(runtime);
        store.remove_files().unwrap();
        let _ = std::fs::remove_file(base.with_extension("lock"));
        let _ = std::fs::remove_file(crate::transport_identity_path(&base));
        let _ = std::fs::remove_file(base.with_extension("transport.lock"));
    }
}

#[tokio::test]
async fn consensus_startup_storage_wait_does_not_hold_the_listener_task() {
    let (store, base) = temp_store();
    store
        .initialize(&SecondState::genesis([], 1), &validator_set())
        .unwrap();
    let runtime = Arc::new(
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
    let client = QuicClient::new(
        "127.0.0.1:0".parse().unwrap(),
        runtime.transport_certificate_der(),
        QuicTransportIdentity::generate().unwrap(),
    )
    .unwrap();
    let held = Arc::new(AtomicBool::new(false));
    let writer_held = held.clone();
    let lock_base = base.clone();
    let (locked, ready) = tokio::sync::oneshot::channel();
    let writer = std::thread::spawn(move || {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(lock_base)
            .unwrap();
        file.lock().unwrap();
        writer_held.store(true, Ordering::SeqCst);
        locked.send(()).unwrap();
        std::thread::sleep(Duration::from_secs(2));
        writer_held.store(false, Ordering::SeqCst);
        file.unlock().unwrap();
    });
    ready.await.unwrap();
    let running = runtime.clone();
    let worker = tokio::spawn(async move { running.run(&[]).await });
    let peer = client.connect(runtime.local_addr().unwrap()).await.unwrap();
    let pong = client_ping(&peer, 72).await.unwrap();
    let responded_while_locked = held.load(Ordering::SeqCst);
    writer.join().unwrap();
    assert_eq!(pong, runtime.node_id());
    assert!(
        responded_while_locked,
        "the production listener was held by consensus or maintenance storage waiting"
    );
    worker.abort();
    assert!(worker.await.unwrap_err().is_cancelled());
    // Cancellation must drop the listener, rather than leave a detached server.
    assert!(client.connect(runtime.local_addr().unwrap()).await.is_err());
    peer.close();
    client.wait_idle().await;
    drop(runtime);
    store.remove_files().unwrap();
    let _ = std::fs::remove_file(base.with_extension("lock"));
    let _ = std::fs::remove_file(crate::transport_identity_path(&base));
    let _ = std::fs::remove_file(base.with_extension("transport.lock"));
}
