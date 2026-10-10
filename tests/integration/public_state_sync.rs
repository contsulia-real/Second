use crate::support;

use second::{
    CurrencyAddress, NetworkError, NetworkMessage, PublicStateError, SecondState,
    client_sync_public_currency_view, serve_public_currency_connection,
};

#[tokio::test]
async fn one_connection_rebuilds_and_verifies_multi_page_public_view() {
    let state = support::fragmented_public_state(600);
    let expected_summary = state.public_currency_summary();
    let expected_states = state.public_currency_states();
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

    let synced = client_sync_public_currency_view(&peer).await.unwrap();

    assert_eq!(synced.remote_node_id, server_node_id);
    assert_eq!(synced.view.summary, expected_summary);
    assert_eq!(synced.view.states, expected_states);
    assert_eq!(synced.view.states.len(), 601);
    assert_eq!(synced.view.states.first().unwrap().start.value(), 10);
    assert_eq!(synced.view.states.last().unwrap().start.value(), 1809);

    peer.close();
    assert_eq!(server_task.await.unwrap(), client_node_id);
}

#[tokio::test]
async fn sync_rejects_pages_that_do_not_match_the_claimed_summary() {
    let state = SecondState::genesis([], 1).with_reserve(2).unwrap();
    let summary = state.public_currency_summary();
    let mut tampered = state.public_currency_states();
    tampered[0].occupied = false;

    let (server, certificate) = support::quic_server();
    let address = server.local_addr().unwrap();

    let server_task = tokio::spawn(async move {
        let peer = server.accept().await.unwrap();

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
                states: tampered,
                next_start: None,
            })
            .await
            .unwrap();
    });

    let client = support::quic_client(&certificate);
    let peer = client.connect(address).await.unwrap();

    assert_eq!(
        client_sync_public_currency_view(&peer).await,
        Err(NetworkError::PublicState(PublicStateError::SummaryMismatch))
    );

    peer.close();
    server_task.await.unwrap();
}

#[tokio::test]
async fn sync_rejects_non_advancing_page_cursor() {
    let state = support::fragmented_public_state(300);
    let summary = state.public_currency_summary();
    let first_page = state.public_currency_states()[..256].to_vec();
    let last = first_page.last().unwrap().start;

    let (server, certificate) = support::quic_server();
    let address = server.local_addr().unwrap();

    let server_task = tokio::spawn(async move {
        let peer = server.accept().await.unwrap();

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
    let peer = client.connect(address).await.unwrap();

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

#[tokio::test]
async fn sync_accepts_large_supply_with_a_single_range() {
    let state = SecondState::genesis([], 0).with_reserve(u64::MAX).unwrap();
    let expected = state.public_currency_summary();
    let (server, certificate) = support::quic_server();
    let address = server.local_addr().unwrap();
    let server_task = tokio::spawn(async move {
        let peer = server.accept().await.unwrap();
        serve_public_currency_connection(&peer, &state, None)
            .await
            .unwrap();
    });
    let client = support::quic_client(&certificate);
    let peer = client.connect(address).await.unwrap();
    let view = client_sync_public_currency_view(&peer).await.unwrap().view;
    assert_eq!(view.summary, expected);
    assert_eq!(view.states.len(), 1);
    peer.close();
    server_task.await.unwrap();
}
