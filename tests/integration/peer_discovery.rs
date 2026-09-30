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

    let third_record = support::peer_record(&third);
    let bootstrap_to_third = bootstrap.dial(&third_record).await.unwrap();
    assert_eq!(
        client_ping(&bootstrap_to_third, 1).await.unwrap(),
        third.node_id()
    );

    let bootstrap_record = support::peer_record(&bootstrap);
    assert_eq!(first.bootstrap(&[bootstrap_record], 2).await.unwrap(), 2);

    let first_to_bootstrap = first
        .peer(bootstrap.node_id())
        .expect("bootstrap peer must be connected");
    let first_to_third = first
        .peer(third.node_id())
        .expect("peer learned through bootstrap must be connected");
    assert_eq!(
        client_ping(&first_to_bootstrap, 2).await.unwrap(),
        bootstrap.node_id()
    );
    assert_eq!(
        client_ping(&first_to_third, 3).await.unwrap(),
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
