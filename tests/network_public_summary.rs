use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use second::{
    NodeId, SecondState, client_public_currency_summary, serve_public_currency_connection,
};

#[test]
fn real_tcp_summary_matches_local_public_state_and_contains_no_owner_data() {
    let state = SecondState::genesis([], 10).with_reserve(3).unwrap();
    let expected = state.public_currency_summary();

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

    let response = client_public_currency_summary(&mut client, NodeId::from_u64(2)).unwrap();

    assert_eq!(response.remote_node_id, NodeId::from_u64(1));
    assert_eq!(response.summary, expected);
    assert_eq!(response.summary.current_supply, 3);
    assert_eq!(response.summary.reserve_count, 3);
    assert_eq!(response.summary.occupied_count, 0);
    assert_eq!(response.summary.next_currency_address, 13);

    let rendered = format!("{response:?}");
    assert!(!rendered.contains("AccountAddress"));
    assert!(!rendered.to_ascii_lowercase().contains("owner"));

    drop(client);
    assert_eq!(server.join().unwrap(), NodeId::from_u64(2));
}
