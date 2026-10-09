use super::*;
use crate::client_ping;

#[tokio::test]
async fn shared_endpoint_reuses_the_socket_with_independent_certificate_pins() {
    let server = || {
        let identity = QuicTransportIdentity::generate().unwrap();
        let certificate = identity.certificate_der().to_vec();
        (
            QuicServer::bind("127.0.0.1:0".parse().unwrap(), &identity).unwrap(),
            certificate,
        )
    };
    let (source, _) = server();
    let (first, first_pin) = server();
    let (second, second_pin) = server();
    let source_address = source.local_addr().unwrap();
    let first_address = first.local_addr().unwrap();
    let second_address = second.local_addr().unwrap();
    let first_id = first.node_id();
    let second_id = second.node_id();
    let first_client = source
        .shared_client(first_address, &first_pin)
        .unwrap()
        .unwrap();
    let second_client = source
        .shared_client(second_address, &second_pin)
        .unwrap()
        .unwrap();
    assert!(
        source
            .shared_client("[::1]:1".parse().unwrap(), &first_pin)
            .unwrap()
            .is_none()
    );

    let first_worker = tokio::spawn(async move {
        let peer = first.accept().await.unwrap();
        assert_eq!(peer.remote_address(), source_address);
        let requests = tokio::spawn(async move {
            for nonce in [10, 12] {
                let request = peer.accept_request().await.unwrap().unwrap();
                assert_eq!(request.message(), &NetworkMessage::Ping { nonce });
                request
                    .respond(&NetworkMessage::Pong { nonce })
                    .await
                    .unwrap();
            }
            peer
        });
        assert!(
            first.accept().await.is_err(),
            "the other peer's pin must be rejected"
        );
        (first, requests.await.unwrap())
    });
    let second_worker = tokio::spawn(async move {
        let peer = second.accept().await.unwrap();
        assert_eq!(peer.remote_address(), source_address);
        let request = peer.accept_request().await.unwrap().unwrap();
        assert_eq!(request.message(), &NetworkMessage::Ping { nonce: 11 });
        request
            .respond(&NetworkMessage::Pong { nonce: 11 })
            .await
            .unwrap();
        (second, peer)
    });

    // Both handles were configured before either handshake.
    let first_peer = first_client
        .connect_expected(first_address, first_id)
        .await
        .unwrap();
    let second_peer = second_client
        .connect_expected(second_address, second_id)
        .await
        .unwrap();
    assert_eq!(client_ping(&first_peer, 10).await.unwrap(), first_id);
    assert_eq!(client_ping(&second_peer, 11).await.unwrap(), second_id);
    let wrong_pin = source
        .shared_client(first_address, &second_pin)
        .unwrap()
        .unwrap();
    assert!(wrong_pin.connect(first_address).await.is_err());
    assert_eq!(client_ping(&first_peer, 12).await.unwrap(), first_id);
    let _first = first_worker.await.unwrap();
    let _second = second_worker.await.unwrap();
    first_peer.close();
    second_peer.close();
}
