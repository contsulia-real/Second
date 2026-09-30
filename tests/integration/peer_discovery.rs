use std::fs;
use std::net::UdpSocket;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::support;
use second::{NetworkMessage, NodeRuntime, StateStore, client_ping, decode_network_message};

#[tokio::test]
async fn bootstrap_learns_authenticated_peers_and_reuses_them_after_restart() {
    let (first, first_store, first_base) = support::node_runtime_fixture("discovery-first");
    let (bootstrap, bootstrap_store, bootstrap_base) =
        support::node_runtime_fixture("discovery-bootstrap");
    let (third, third_store, third_base) = support::node_runtime_fixture("discovery-third");

    let bootstrap_task = support::spawn_node_runtime(&bootstrap);
    let third_task = support::spawn_node_runtime(&third);

    let bootstrap_record = support::peer_record(&bootstrap);
    assert_eq!(
        first
            .bootstrap(std::slice::from_ref(&bootstrap_record), 1)
            .await
            .unwrap(),
        1
    );

    let first_to_bootstrap = first
        .peer(bootstrap.node_id())
        .expect("bootstrap peer must be connected");
    assert_eq!(
        client_ping(&first_to_bootstrap, 1).await.unwrap(),
        bootstrap.node_id()
    );

    let third_record = support::peer_record(&third);
    let bootstrap_to_third = bootstrap.dial(&third_record).await.unwrap();
    assert_eq!(
        client_ping(&bootstrap_to_third, 2).await.unwrap(),
        third.node_id()
    );

    assert_eq!(first.bootstrap(&[bootstrap_record], 2).await.unwrap(), 2);

    let first_to_third = first
        .peer(third.node_id())
        .expect("peer learned through bootstrap must be connected");
    assert_eq!(
        client_ping(&first_to_bootstrap, 3).await.unwrap(),
        bootstrap.node_id()
    );
    assert_eq!(
        client_ping(&first_to_third, 4).await.unwrap(),
        third.node_id()
    );

    first_to_bootstrap.close();
    first_to_third.close();
    drop(first);

    let restarted =
        Arc::new(NodeRuntime::load_and_bind("127.0.0.1:0".parse().unwrap(), &first_store).unwrap());
    assert_eq!(restarted.bootstrap(&[], 2).await.unwrap(), 2);
    assert!(restarted.peer(bootstrap.node_id()).is_some());
    assert!(restarted.peer(third.node_id()).is_some());

    bootstrap_to_third.close();
    if let Some(peer) = restarted.peer(bootstrap.node_id()) {
        peer.close();
    }
    if let Some(peer) = restarted.peer(third.node_id()) {
        peer.close();
    }

    bootstrap_task.abort();
    third_task.abort();
    let _ = bootstrap_task.await;
    let _ = third_task.await;
    drop(restarted);
    drop(bootstrap);
    drop(third);

    support::cleanup_node_runtime(first_store, first_base);
    support::cleanup_node_runtime(bootstrap_store, bootstrap_base);
    support::cleanup_node_runtime(third_store, third_base);
}

#[tokio::test]
async fn runtime_maintains_bootstrap_connection_beyond_quic_idle_timeout() {
    let (client, client_store, client_base) = support::node_runtime_fixture("maintenance-client");
    let (seed, seed_store, seed_base) = support::node_runtime_fixture("maintenance-seed");

    let seed_task = support::spawn_node_runtime(&seed);
    let client_task =
        support::spawn_node_runtime_with_bootstrap(&client, vec![support::peer_record(&seed)]);

    tokio::time::sleep(std::time::Duration::from_secs(7)).await;

    let peer = client
        .peer(seed.node_id())
        .expect("maintained bootstrap peer must remain connected");
    assert_eq!(client_ping(&peer, 9).await.unwrap(), seed.node_id());

    peer.close();
    tokio::time::sleep(std::time::Duration::from_secs(4)).await;

    let reconnected = client
        .peer(seed.node_id())
        .expect("maintenance must reconnect a dropped bootstrap peer");
    assert_eq!(client_ping(&reconnected, 10).await.unwrap(), seed.node_id());

    client_task.abort();
    seed_task.abort();
    let _ = client_task.await;
    let _ = seed_task.await;
    drop(client);
    drop(seed);

    support::cleanup_node_runtime(client_store, client_base);
    support::cleanup_node_runtime(seed_store, seed_base);
}

#[tokio::test]
async fn authenticated_peer_refresh_replaces_stale_reachability_after_restart() {
    let first_base = support::temp_base("reachability-first");
    let relay_base = support::temp_base("reachability-relay");
    let first_store = StateStore::new(&first_base);
    let relay_store = StateStore::new(&relay_base);
    let state = second::SecondState::genesis([], 1).with_reserve(2).unwrap();
    let validators = support::validator_set(1, 1..=4);
    first_store.initialize(&state, &validators).unwrap();
    relay_store.initialize(&state, &validators).unwrap();

    let old_address_guard = UdpSocket::bind("127.0.0.1:0").unwrap();
    let old_address = old_address_guard.local_addr().unwrap();
    let new_address_guard = UdpSocket::bind("127.0.0.1:0").unwrap();
    let new_address = new_address_guard.local_addr().unwrap();
    drop(old_address_guard);

    let first = Arc::new(NodeRuntime::load_and_bind(old_address, &first_store).unwrap());
    let relay =
        Arc::new(NodeRuntime::load_and_bind("127.0.0.1:0".parse().unwrap(), &relay_store).unwrap());
    let node_id = first.node_id();
    assert_eq!(first.local_addr().unwrap(), old_address);
    let relay_record = support::peer_record(&relay);
    let first_task = support::spawn_node_runtime(&first);
    let relay_task = support::spawn_node_runtime(&relay);

    let old_peer = first.dial(&relay_record).await.unwrap();
    assert_eq!(client_ping(&old_peer, 20).await.unwrap(), relay.node_id());
    wait_for_persisted_peer_address(&relay_base, node_id, old_address).await;

    old_peer.close();
    first_task.abort();
    let _ = first_task.await;
    drop(first);

    drop(new_address_guard);
    let restarted = Arc::new(NodeRuntime::load_and_bind(new_address, &first_store).unwrap());
    assert_eq!(restarted.node_id(), node_id);
    assert_eq!(restarted.local_addr().unwrap(), new_address);
    let restarted_task = support::spawn_node_runtime(&restarted);

    let new_peer = restarted.dial(&relay_record).await.unwrap();
    assert_eq!(client_ping(&new_peer, 21).await.unwrap(), relay.node_id());
    wait_for_persisted_peer_address(&relay_base, node_id, new_address).await;

    new_peer.close();
    relay_task.abort();
    let _ = relay_task.await;
    drop(relay);

    let relay_restarted =
        Arc::new(NodeRuntime::load_and_bind("127.0.0.1:0".parse().unwrap(), &relay_store).unwrap());
    assert_eq!(relay_restarted.bootstrap(&[], 1).await.unwrap(), 1);
    let refreshed = relay_restarted
        .peer(node_id)
        .expect("relay must dial the refreshed peer address");
    assert_eq!(client_ping(&refreshed, 22).await.unwrap(), node_id);

    refreshed.close();
    restarted_task.abort();
    let _ = restarted_task.await;
    drop(relay_restarted);
    drop(restarted);

    support::cleanup_node_runtime(first_store, first_base);
    support::cleanup_node_runtime(relay_store, relay_base);
}

async fn wait_for_persisted_peer_address(
    base: &std::path::Path,
    node_id: second::NodeId,
    address: std::net::SocketAddr,
) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Ok(bytes) = fs::read(support::peer_store_path(base))
            && let Ok(NetworkMessage::Peers { records }) = decode_network_message(&bytes)
            && records
                .iter()
                .any(|record| record.node_id() == node_id && record.address() == address)
        {
            return;
        }

        assert!(
            Instant::now() < deadline,
            "peer store did not persist refreshed reachability {node_id} at {address}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn failed_persisted_peer_is_demoted_behind_recent_success() {
    let (client, client_store, client_base) = support::node_runtime_fixture("quality-client");
    let (good, good_store, good_base) = support::node_runtime_fixture("quality-good");
    let (bad, bad_store, bad_base) = support::node_runtime_fixture("quality-bad");

    let good_task = support::spawn_node_runtime(&good);
    let bad_task = support::spawn_node_runtime(&bad);
    let good_record = support::peer_record(&good);
    let bad_record = support::peer_record(&bad);

    let good_peer = client.dial(&good_record).await.unwrap();
    wait_for_persisted_peer_address(&client_base, good.node_id(), good_record.address()).await;
    let bad_peer = client.dial(&bad_record).await.unwrap();
    wait_for_persisted_peer_address(&client_base, bad.node_id(), bad_record.address()).await;

    good_peer.close();
    bad_peer.close();
    drop(client);

    bad_task.abort();
    let _ = bad_task.await;
    drop(bad);

    let restarted = Arc::new(
        NodeRuntime::load_and_bind("127.0.0.1:0".parse().unwrap(), &client_store).unwrap(),
    );
    assert_eq!(restarted.bootstrap(&[], 1).await.unwrap(), 1);
    assert!(restarted.peer(good.node_id()).is_some());

    let records = persisted_peer_records(&client_base);
    assert_eq!(
        records.first().map(|record| record.node_id()),
        Some(bad_record.node_id())
    );
    assert_eq!(
        records.last().map(|record| record.node_id()),
        Some(good_record.node_id())
    );

    if let Some(peer) = restarted.peer(good.node_id()) {
        peer.close();
    }
    good_task.abort();
    let _ = good_task.await;
    drop(restarted);
    drop(good);

    support::cleanup_node_runtime(client_store, client_base);
    support::cleanup_node_runtime(good_store, good_base);
    support::cleanup_node_runtime(bad_store, bad_base);
}

fn persisted_peer_records(base: &std::path::Path) -> Vec<second::PeerRecord> {
    let bytes = fs::read(support::peer_store_path(base)).unwrap();
    match decode_network_message(&bytes).unwrap() {
        NetworkMessage::Peers { records } => records,
        other => panic!("unexpected peer store payload: {other:?}"),
    }
}
