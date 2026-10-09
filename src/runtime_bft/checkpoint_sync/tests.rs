//! A fresh dial's checkpoint replay must not hold the network executor on storage.
use super::*;
use crate::prepared::tests::{key, temp_store, validator_set};
use crate::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[tokio::test]
async fn checkpoint_replay_storage_wait_does_not_block_another_connection() {
    let (store, base) = temp_store();
    let validators = validator_set();
    store
        .initialize(&SecondState::genesis([], 1), &validators)
        .unwrap();
    let node =
        Arc::new(NodeRuntime::load_and_bind("127.0.0.1:0".parse().unwrap(), &store).unwrap());
    let running = node.clone();
    let worker = tokio::spawn(async move { running.run(&[]).await });
    let client = QuicClient::new(
        "127.0.0.1:0".parse().unwrap(),
        node.transport_certificate_der(),
        QuicTransportIdentity::generate().unwrap(),
    )
    .unwrap();
    let peer = client.connect(node.local_addr().unwrap()).await.unwrap();
    client_ping(&peer, 91).await.unwrap();
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
        [],
    )
    .unwrap();
    let held = Arc::new(AtomicBool::new(false));
    let holding = held.clone();
    let lock_base = base.clone();
    let (locked, ready) = tokio::sync::oneshot::channel();
    let writer = std::thread::spawn(move || {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(lock_base)
            .unwrap();
        file.lock().unwrap();
        holding.store(true, Ordering::SeqCst);
        locked.send(()).unwrap();
        std::thread::sleep(Duration::from_secs(2));
        holding.store(false, Ordering::SeqCst);
        file.unlock().unwrap();
    });
    ready.await.unwrap();
    let replay = tokio::spawn(async move {
        runtime.sync_checkpoints(ValidatorId::new(2)).await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(client_ping(&peer, 92).await.unwrap(), node.node_id());
    let responded_while_locked = held.load(Ordering::SeqCst);
    writer.join().unwrap();
    replay.await.unwrap();
    peer.close();
    worker.abort();
    let _ = worker.await;
    client.wait_idle().await;
    drop(node);
    assert!(
        responded_while_locked,
        "checkpoint replay blocked another connection until storage unlocked"
    );
    store.remove_files().unwrap();
    for suffix in ["transport", "transport.lock", "peers"] {
        let _ = std::fs::remove_file(base.with_extension(suffix));
    }
}
