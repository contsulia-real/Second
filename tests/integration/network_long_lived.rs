use crate::support;

use second::{
    CurrencyAddress, NetworkError, NetworkMessage, SecondState, client_ping,
    client_public_currency_page, serve_public_currency_connection,
};

#[tokio::test]
async fn delivered_request_without_response_reports_the_response_deadline() {
    let (server, certificate) = support::quic_server();
    let address = server.local_addr().unwrap();
    let (delivered, delivery) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let server_task = tokio::spawn(async move {
        let peer = server.accept().await.unwrap();
        let request = peer.accept_request().await.unwrap().unwrap();
        assert_eq!(request.message(), &NetworkMessage::Ping { nonce: 77 });
        delivered.send(()).unwrap();
        let _ = released.await;
        drop(request);
    });
    let client = support::quic_client(&certificate);
    let peer = client.connect(address).await.unwrap();
    let result = peer.exchange(&NetworkMessage::Ping { nonce: 77 }).await;
    delivery.await.unwrap();
    assert!(
        matches!(result, Err(NetworkError::Transport(ref error))
        if error == "protocol request timed out while awaiting response"),
        "{result:?}"
    );
    release.send(()).unwrap();
    server_task.await.unwrap();
    peer.close();
    client.wait_idle().await;
}

#[tokio::test]
async fn one_quic_connection_supports_multiple_independent_request_streams() {
    let state = SecondState::genesis([], 1).with_reserve(3).unwrap();
    let (server, certificate) = support::quic_server();
    let address = server.local_addr().unwrap();
    let server_node_id = server.node_id();

    let server_task = tokio::spawn(async move {
        let peer = server.accept().await.unwrap();
        serve_public_currency_connection(&peer, &state, None)
            .await
            .unwrap()
    });

    let client = support::quic_client(&certificate);
    let client_node_id = client.node_id();
    let peer = client.connect(address).await.unwrap();

    let first = client_public_currency_page(&peer, CurrencyAddress::new(1), 1)
        .await
        .unwrap();
    assert_eq!(first.states.len(), 1);
    assert_eq!(first.states[0].start, CurrencyAddress::new(1));
    assert_eq!(first.states[0].len, 3);
    assert_eq!(first.next_start, None);

    assert_eq!(client_ping(&peer, 77).await.unwrap(), server_node_id);

    let second = client_public_currency_page(&peer, CurrencyAddress::new(2), 2)
        .await
        .unwrap();
    assert_eq!(second.states.len(), 1);
    assert_eq!(second.states[0].start, CurrencyAddress::new(2));
    assert_eq!(second.states[0].len, 2);
    assert_eq!(second.next_start, None);

    peer.close();
    assert_eq!(server_task.await.unwrap(), client_node_id);
}
