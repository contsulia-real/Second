use crate::support;

use second::{
    NodeId, SecondState, client_public_currency_summary, serve_public_currency_connection,
};

#[tokio::test]
async fn real_quic_summary_matches_local_public_state_and_contains_no_owner_data() {
    let state = SecondState::genesis([], 10).with_reserve(3).unwrap();
    let expected = state.public_currency_summary();
    let (server, certificate) = support::quic_server();
    let address = server.local_addr().unwrap();

    let server_task = tokio::spawn(async move {
        let peer = server.accept(NodeId::from_u64(1)).await.unwrap();
        serve_public_currency_connection(&peer, &state, None)
            .await
            .unwrap()
    });

    let client = support::quic_client(&certificate);
    let peer = client.connect(address, NodeId::from_u64(2)).await.unwrap();

    let response = client_public_currency_summary(&peer).await.unwrap();

    assert_eq!(response.remote_node_id, NodeId::from_u64(1));
    assert_eq!(response.summary, expected);
    assert_eq!(response.summary.current_supply, 3);
    assert_eq!(response.summary.reserve_count, 3);
    assert_eq!(response.summary.occupied_count, 0);
    assert_eq!(response.summary.next_currency_address, 13);

    let rendered = format!("{response:?}");
    assert!(!rendered.contains("AccountAddress"));
    assert!(!rendered.to_ascii_lowercase().contains("owner"));

    peer.close();
    assert_eq!(server_task.await.unwrap(), NodeId::from_u64(2));
}
