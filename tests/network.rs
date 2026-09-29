use std::io::Cursor;
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use second::{
    MAX_NETWORK_FRAME_SIZE, NetworkError, NetworkMessage, NodeId, client_ping,
    read_network_message, serve_ping_session, write_network_message,
};

#[test]
fn network_frames_round_trip_without_json_or_platform_dependent_layout() {
    let messages = [
        NetworkMessage::Hello {
            node_id: NodeId::from_u64(7),
        },
        NetworkMessage::Ping { nonce: 42 },
        NetworkMessage::Pong { nonce: 42 },
    ];

    for message in messages {
        let mut bytes = Vec::new();
        write_network_message(&mut bytes, &message).unwrap();

        let decoded = read_network_message(&mut Cursor::new(bytes)).unwrap();
        assert_eq!(decoded, message);
    }
}

#[test]
fn oversized_frames_are_rejected_before_payload_allocation() {
    let mut frame = Vec::new();
    frame.extend_from_slice(b"SCND");
    frame.extend_from_slice(&second::CURRENT_PROTOCOL_VERSION.to_be_bytes());
    frame.extend_from_slice(&((MAX_NETWORK_FRAME_SIZE as u32) + 1).to_be_bytes());

    assert_eq!(
        read_network_message(&mut Cursor::new(frame)),
        Err(NetworkError::FrameTooLarge {
            announced: MAX_NETWORK_FRAME_SIZE + 1,
            maximum: MAX_NETWORK_FRAME_SIZE,
        })
    );
}

#[test]
fn protocol_version_mismatch_is_rejected_during_frame_read() {
    let mut frame = Vec::new();
    frame.extend_from_slice(b"SCND");
    frame.extend_from_slice(&(second::CURRENT_PROTOCOL_VERSION + 1).to_be_bytes());
    frame.extend_from_slice(&1_u32.to_be_bytes());
    frame.push(2);

    assert_eq!(
        read_network_message(&mut Cursor::new(frame)),
        Err(NetworkError::UnsupportedProtocolVersion {
            expected: second::CURRENT_PROTOCOL_VERSION,
            actual: second::CURRENT_PROTOCOL_VERSION + 1,
        })
    );
}

#[test]
fn two_real_tcp_nodes_complete_handshake_and_ping() {
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
        serve_ping_session(&mut stream, NodeId::from_u64(1)).unwrap()
    });

    let mut client = TcpStream::connect(address).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    client
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();

    let remote = client_ping(&mut client, NodeId::from_u64(2), 99).unwrap();
    assert_eq!(remote, NodeId::from_u64(1));

    server.join().unwrap();
}
