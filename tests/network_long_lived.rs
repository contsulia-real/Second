mod support;

use second::{
    CurrencyAddress, NodeId, SecondState, client_ping, client_public_currency_page,
    serve_public_currency_connection,
};

#[tokio::test]
async fn one_quic_connection_supports_multiple_independent_request_streams() {
    let state = SecondState::genesis([], 1).with_reserve(3).unwrap();
    let (server, certificate) = support::quic_server();
    let address = server.local_addr().unwrap();

    let server_task = tokio::spawn(async move {
        let peer = server.accept(NodeId::from_u64(1)).await.unwrap();
        serve_public_currency_connection(&peer, &state)
            .await
            .unwrap()
    });

    let client = support::quic_client(&certificate);
    let peer = client.connect(address, NodeId::from_u64(2)).await.unwrap();

    let first = client_public_currency_page(&peer, CurrencyAddress::new(1), 1)
        .await
        .unwrap();
    assert_eq!(first.states.len(), 1);
    assert_eq!(first.states[0].address, CurrencyAddress::new(1));
    assert_eq!(first.next_start, Some(CurrencyAddress::new(2)));

    assert_eq!(client_ping(&peer, 77).await.unwrap(), NodeId::from_u64(1));

    let second = client_public_currency_page(&peer, CurrencyAddress::new(2), 2)
        .await
        .unwrap();
    assert_eq!(second.states.len(), 2);
    assert_eq!(second.states[0].address, CurrencyAddress::new(2));
    assert_eq!(second.states[1].address, CurrencyAddress::new(3));
    assert_eq!(second.next_start, None);

    peer.close();
    assert_eq!(server_task.await.unwrap(), NodeId::from_u64(2));
}
