use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use second::{
    CurrencyAddress, CurrencyRole, NetworkError, NetworkMessage, NodeId, PublicStateError,
    SecondState, client_sync_public_currency_view, read_network_message,
    serve_public_currency_connection, write_network_message,
};

#[test]
fn one_connection_rebuilds_and_verifies_multi_page_public_view() {
    let state = SecondState::genesis([], 10).with_reserve(600).unwrap();
    let expected_summary = state.public_currency_summary();
    let expected_states = state.public_currency_states();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();

        serve_public_currency_connection(&mut stream, NodeId::from_u64(1), &state).unwrap()
    });

    let mut client = TcpStream::connect(address).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    client
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();

    let synced = client_sync_public_currency_view(&mut client, NodeId::from_u64(2)).unwrap();

    assert_eq!(synced.remote_node_id, NodeId::from_u64(1));
    assert_eq!(synced.view.summary, expected_summary);
    assert_eq!(synced.view.states, expected_states);
    assert_eq!(synced.view.states.len(), 600);
    assert_eq!(synced.view.states.first().unwrap().address.value(), 10);
    assert_eq!(synced.view.states.last().unwrap().address.value(), 609);

    drop(client);
    assert_eq!(server.join().unwrap(), NodeId::from_u64(2));
}

fn accept_handshake(stream: &mut TcpStream, local_node_id: NodeId) {
    assert!(matches!(
        read_network_message(stream).unwrap(),
        NetworkMessage::Hello { .. }
    ));
    write_network_message(
        stream,
        &NetworkMessage::Hello {
            node_id: local_node_id,
        },
    )
    .unwrap();
}

#[test]
fn sync_rejects_pages_that_do_not_match_the_claimed_summary() {
    let state = SecondState::genesis([], 1).with_reserve(2).unwrap();
    let summary = state.public_currency_summary();
    let mut tampered = state.public_currency_states();
    tampered[0].role = CurrencyRole::Circulation;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        accept_handshake(&mut stream, NodeId::from_u64(1));

        assert_eq!(
            read_network_message(&mut stream).unwrap(),
            NetworkMessage::GetPublicCurrencySummary
        );
        write_network_message(
            &mut stream,
            &NetworkMessage::PublicCurrencySummary { summary },
        )
        .unwrap();

        assert_eq!(
            read_network_message(&mut stream).unwrap(),
            NetworkMessage::GetPublicCurrencies {
                start: CurrencyAddress::new(0),
                limit: 2,
            }
        );
        write_network_message(
            &mut stream,
            &NetworkMessage::PublicCurrencies {
                states: tampered,
                next_start: None,
            },
        )
        .unwrap();
    });

    let mut client = TcpStream::connect(address).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    client
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();

    assert_eq!(
        client_sync_public_currency_view(&mut client, NodeId::from_u64(2)),
        Err(NetworkError::PublicState(PublicStateError::SummaryMismatch))
    );

    server.join().unwrap();
}

#[test]
fn sync_rejects_non_advancing_page_cursor() {
    let state = SecondState::genesis([], 1).with_reserve(300).unwrap();
    let summary = state.public_currency_summary();
    let first_page = state.public_currency_states()[..256].to_vec();
    let last = first_page.last().unwrap().address;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        accept_handshake(&mut stream, NodeId::from_u64(1));

        assert_eq!(
            read_network_message(&mut stream).unwrap(),
            NetworkMessage::GetPublicCurrencySummary
        );
        write_network_message(
            &mut stream,
            &NetworkMessage::PublicCurrencySummary { summary },
        )
        .unwrap();

        assert_eq!(
            read_network_message(&mut stream).unwrap(),
            NetworkMessage::GetPublicCurrencies {
                start: CurrencyAddress::new(0),
                limit: 256,
            }
        );
        write_network_message(
            &mut stream,
            &NetworkMessage::PublicCurrencies {
                states: first_page,
                next_start: Some(last),
            },
        )
        .unwrap();
    });

    let mut client = TcpStream::connect(address).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    client
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();

    assert_eq!(
        client_sync_public_currency_view(&mut client, NodeId::from_u64(2)),
        Err(NetworkError::InvalidPublicCurrencyCursor {
            current: last,
            next: last,
        })
    );

    server.join().unwrap();
}
