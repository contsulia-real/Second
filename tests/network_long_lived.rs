use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use second::{
    CurrencyAddress, NetworkMessage, NodeId, SecondState, read_network_message,
    serve_public_currency_connection, write_network_message,
};

#[test]
fn one_handshake_supports_multiple_requests_until_clean_disconnect() {
    let state = SecondState::genesis([], 1).with_reserve(3).unwrap();

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

    write_network_message(
        &mut client,
        &NetworkMessage::Hello {
            node_id: NodeId::from_u64(2),
        },
    )
    .unwrap();

    assert_eq!(
        read_network_message(&mut client).unwrap(),
        NetworkMessage::Hello {
            node_id: NodeId::from_u64(1),
        }
    );

    write_network_message(
        &mut client,
        &NetworkMessage::GetPublicCurrencies {
            start: CurrencyAddress::new(1),
            span: 1,
        },
    )
    .unwrap();

    match read_network_message(&mut client).unwrap() {
        NetworkMessage::PublicCurrencies { states, next_start } => {
            assert_eq!(states.len(), 1);
            assert_eq!(states[0].address, CurrencyAddress::new(1));
            assert_eq!(next_start, Some(CurrencyAddress::new(2)));
        }
        other => panic!("unexpected response: {other:?}"),
    }

    write_network_message(&mut client, &NetworkMessage::Ping { nonce: 77 }).unwrap();
    assert_eq!(
        read_network_message(&mut client).unwrap(),
        NetworkMessage::Pong { nonce: 77 }
    );

    write_network_message(
        &mut client,
        &NetworkMessage::GetPublicCurrencies {
            start: CurrencyAddress::new(2),
            span: 2,
        },
    )
    .unwrap();

    match read_network_message(&mut client).unwrap() {
        NetworkMessage::PublicCurrencies { states, next_start } => {
            assert_eq!(states.len(), 2);
            assert_eq!(states[0].address, CurrencyAddress::new(2));
            assert_eq!(states[1].address, CurrencyAddress::new(3));
            assert_eq!(next_start, None);
        }
        other => panic!("unexpected response: {other:?}"),
    }

    drop(client);

    assert_eq!(server.join().unwrap(), NodeId::from_u64(2));
}
