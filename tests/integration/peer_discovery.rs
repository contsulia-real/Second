use std::sync::Arc;

use crate::support;
use second::{NodeRuntime, client_ping};

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
