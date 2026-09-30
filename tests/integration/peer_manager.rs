use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use crate::support;
use second::{
    NetworkError, NodeId, NodeRuntime, NodeRuntimeError, QuicClient, QuicTransportIdentity,
    SecondState, StateStore, client_ping,
};

fn runtime_fixture(prefix: &str) -> (Arc<NodeRuntime>, StateStore, PathBuf) {
    let base = support::temp_base(prefix);
    let store = StateStore::new(&base);
    let state = SecondState::genesis([], 1).with_reserve(2).unwrap();
    store
        .initialize(&state, &support::validator_set(1, 1..=4))
        .unwrap();

    let runtime = Arc::new(
        NodeRuntime::load_and_bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), &store).unwrap(),
    );
    (runtime, store, base)
}

fn spawn_runtime(runtime: &Arc<NodeRuntime>) -> tokio::task::JoinHandle<()> {
    let runtime = Arc::clone(runtime);
    tokio::spawn(async move {
        let _ = runtime.run().await;
    })
}

fn cleanup_fixture(store: StateStore, base: PathBuf) {
    store.remove_files().unwrap();
    support::remove_transport_identity(&base);
}

#[tokio::test]
async fn runtime_keeps_only_one_active_connection_per_authenticated_node_id() {
    let (runtime, store, base) = runtime_fixture("peer-manager");
    let address = runtime.local_addr().unwrap();
    let server_node_id = runtime.node_id();
    let certificate = runtime.transport_certificate_der().to_vec();
    let runtime_task = spawn_runtime(&runtime);

    let identity = QuicTransportIdentity::generate().unwrap();
    let first_client = QuicClient::new(
        SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
        &certificate,
        identity.clone(),
    )
    .unwrap();
    let first_peer = first_client.connect(address).await.unwrap();
    assert_eq!(client_ping(&first_peer, 1).await.unwrap(), server_node_id);

    let duplicate_client = QuicClient::new(
        SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
        &certificate,
        identity,
    )
    .unwrap();
    let duplicate_peer = duplicate_client.connect(address).await.unwrap();
    assert!(client_ping(&duplicate_peer, 2).await.is_err());

    assert_eq!(client_ping(&first_peer, 3).await.unwrap(), server_node_id);

    first_peer.close();
    duplicate_peer.close();
    first_client.wait_idle().await;
    duplicate_client.wait_idle().await;

    runtime_task.abort();
    let _ = runtime_task.await;
    drop(runtime);

    cleanup_fixture(store, base);
}

#[tokio::test]
async fn runtime_outbound_dial_rejects_unexpected_authenticated_node_id() {
    let (caller, caller_store, caller_base) = runtime_fixture("outbound-caller");
    let (target, target_store, target_base) = runtime_fixture("outbound-target");
    let target_address = target.local_addr().unwrap();
    let target_node_id = target.node_id();
    let target_certificate = target.transport_certificate_der().to_vec();
    let target_task = spawn_runtime(&target);

    let mut wrong_bytes = target_node_id.to_bytes();
    wrong_bytes[0] ^= 1;
    let wrong_node_id = NodeId::from_bytes(wrong_bytes);

    assert!(matches!(
        caller
            .dial(target_address, wrong_node_id, &target_certificate)
            .await,
        Err(NodeRuntimeError::Network(
            NetworkError::UnexpectedPeerIdentity { expected, actual }
        )) if expected == wrong_node_id && actual == target_node_id
    ));

    target_task.abort();
    let _ = target_task.await;
    drop(caller);
    drop(target);

    cleanup_fixture(caller_store, caller_base);
    cleanup_fixture(target_store, target_base);
}

#[tokio::test]
async fn simultaneous_dial_converges_on_one_bidirectional_connection() {
    let (first, first_store, first_base) = runtime_fixture("simultaneous-first");
    let (second, second_store, second_base) = runtime_fixture("simultaneous-second");

    let first_address = first.local_addr().unwrap();
    let second_address = second.local_addr().unwrap();
    let first_certificate = first.transport_certificate_der().to_vec();
    let second_certificate = second.transport_certificate_der().to_vec();

    let first_task = spawn_runtime(&first);
    let second_task = spawn_runtime(&second);

    let (low, low_address, low_certificate, high, high_address, high_certificate) =
        if first.node_id() < second.node_id() {
            (
                &first,
                first_address,
                &first_certificate,
                &second,
                second_address,
                &second_certificate,
            )
        } else {
            (
                &second,
                second_address,
                &second_certificate,
                &first,
                first_address,
                &first_certificate,
            )
        };

    let (low_to_high, high_to_low) = tokio::join!(
        low.dial(high_address, high.node_id(), high_certificate),
        high.dial(low_address, low.node_id(), low_certificate),
    );

    let low_peer = low_to_high.unwrap();
    let _high_dial_peer = high_to_low.unwrap();

    assert_eq!(
        client_ping(&low_peer, 10).await.unwrap(),
        high.node_id(),
        "the canonical lower-NodeId outbound connection must stay usable"
    );

    let high_active_peer = high
        .peer(low.node_id())
        .expect("higher NodeId runtime must retain the canonical inbound peer");
    assert_eq!(
        client_ping(&high_active_peer, 11).await.unwrap(),
        low.node_id(),
        "both nodes must initiate requests over the one retained connection"
    );

    low_peer.close();

    first_task.abort();
    second_task.abort();
    let _ = first_task.await;
    let _ = second_task.await;
    drop(first);
    drop(second);

    cleanup_fixture(first_store, first_base);
    cleanup_fixture(second_store, second_base);
}
