use std::net::{Ipv4Addr, SocketAddr};

use crate::support;
use second::{
    NodeRuntime, QuicClient, QuicTransportIdentity, SecondState, StateStore, client_ping,
};

#[tokio::test]
async fn runtime_keeps_only_one_active_connection_per_authenticated_node_id() {
    let base = support::temp_base("peer-manager");
    let store = StateStore::new(&base);
    let state = SecondState::genesis([], 1).with_reserve(2).unwrap();
    store
        .initialize(&state, &support::validator_set(1, 1..=4))
        .unwrap();

    let runtime =
        NodeRuntime::load_and_bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), &store).unwrap();
    let address = runtime.local_addr().unwrap();
    let server_node_id = runtime.node_id();
    let certificate = runtime.transport_certificate_der().to_vec();
    let runtime_task = tokio::spawn(runtime.run());

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

    store.remove_files().unwrap();
    support::remove_transport_identity(&base);
}
