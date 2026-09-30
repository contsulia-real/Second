mod support;

use second::{
    CurrencyAddress, CurrencyRole, NetworkError, NetworkMessage, NodeId, PublicStateError,
    SecondState, client_sync_public_currency_view, serve_public_currency_connection,
};

#[tokio::test]
async fn one_connection_rebuilds_and_verifies_multi_page_public_view() {
    let state = SecondState::genesis([], 10).with_reserve(600).unwrap();
    let expected_summary = state.public_currency_summary();
    let expected_states = state.public_currency_states();
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

    let synced = client_sync_public_currency_view(&peer).await.unwrap();

    assert_eq!(synced.remote_node_id, NodeId::from_u64(1));
    assert_eq!(synced.view.summary, expected_summary);
    assert_eq!(synced.view.states, expected_states);
    assert_eq!(synced.view.states.len(), 600);
    assert_eq!(synced.view.states.first().unwrap().address.value(), 10);
    assert_eq!(synced.view.states.last().unwrap().address.value(), 609);

    peer.close();
    assert_eq!(server_task.await.unwrap(), NodeId::from_u64(2));
}

#[tokio::test]
async fn sync_rejects_pages_that_do_not_match_the_claimed_summary() {
    let state = SecondState::genesis([], 1).with_reserve(2).unwrap();
    let summary = state.public_currency_summary();
    let mut tampered = state.public_currency_states();
    tampered[0].role = CurrencyRole::Circulation;

    let (server, certificate) = support::quic_server();
    let address = server.local_addr().unwrap();

    let server_task = tokio::spawn(async move {
        let peer = server.accept(NodeId::from_u64(1)).await.unwrap();

        let request = peer.accept_request().await.unwrap().unwrap();
        assert_eq!(request.message(), &NetworkMessage::GetPublicCurrencySummary);
        request
            .respond(&NetworkMessage::PublicCurrencySummary { summary })
            .await
            .unwrap();

        let request = peer.accept_request().await.unwrap().unwrap();
        assert_eq!(
            request.message(),
            &NetworkMessage::GetPublicCurrencies {
                start: CurrencyAddress::new(0),
                limit: 2,
            }
        );
        request
            .respond(&NetworkMessage::PublicCurrencies {
                states: tampered,
                next_start: None,
            })
            .await
            .unwrap();
    });

    let client = support::quic_client(&certificate);
    let peer = client.connect(address, NodeId::from_u64(2)).await.unwrap();

    assert_eq!(
        client_sync_public_currency_view(&peer).await,
        Err(NetworkError::PublicState(PublicStateError::SummaryMismatch))
    );

    peer.close();
    server_task.await.unwrap();
}

#[tokio::test]
async fn sync_rejects_non_advancing_page_cursor() {
    let state = SecondState::genesis([], 1).with_reserve(300).unwrap();
    let summary = state.public_currency_summary();
    let first_page = state.public_currency_states()[..256].to_vec();
    let last = first_page.last().unwrap().address;

    let (server, certificate) = support::quic_server();
    let address = server.local_addr().unwrap();

    let server_task = tokio::spawn(async move {
        let peer = server.accept(NodeId::from_u64(1)).await.unwrap();

        let request = peer.accept_request().await.unwrap().unwrap();
        assert_eq!(request.message(), &NetworkMessage::GetPublicCurrencySummary);
        request
            .respond(&NetworkMessage::PublicCurrencySummary { summary })
            .await
            .unwrap();

        let request = peer.accept_request().await.unwrap().unwrap();
        assert_eq!(
            request.message(),
            &NetworkMessage::GetPublicCurrencies {
                start: CurrencyAddress::new(0),
                limit: 256,
            }
        );
        request
            .respond(&NetworkMessage::PublicCurrencies {
                states: first_page,
                next_start: Some(last),
            })
            .await
            .unwrap();
    });

    let client = support::quic_client(&certificate);
    let peer = client.connect(address, NodeId::from_u64(2)).await.unwrap();

    assert_eq!(
        client_sync_public_currency_view(&peer).await,
        Err(NetworkError::InvalidPublicCurrencyCursor {
            current: last,
            next: last,
        })
    );

    peer.close();
    server_task.await.unwrap();
}
