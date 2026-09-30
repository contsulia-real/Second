use crate::support;

use second::{
    MAX_NETWORK_FRAME_SIZE, NetworkError, NetworkMessage, NodeId, client_ping,
    decode_network_message, encode_network_message, serve_ping_session,
};

#[test]
fn network_frames_round_trip_without_json_or_platform_dependent_layout() {
    let messages = [
        NetworkMessage::Hello {
            node_id: NodeId::from_bytes([7; 32]),
            signature: [9; 64],
        },
        NetworkMessage::Ping { nonce: 42 },
        NetworkMessage::Pong { nonce: 42 },
    ];

    for message in messages {
        let bytes = encode_network_message(&message).unwrap();
        let decoded = decode_network_message(&bytes).unwrap();
        assert_eq!(decoded, message);
    }
}

#[test]
fn oversized_frames_are_rejected_before_payload_allocation() {
    let mut frame = Vec::new();
    frame.extend_from_slice(b"SCND");
    frame.extend_from_slice(&second::CURRENT_NETWORK_PROTOCOL_VERSION.to_be_bytes());
    frame.extend_from_slice(&((MAX_NETWORK_FRAME_SIZE as u32) + 1).to_be_bytes());

    assert_eq!(
        decode_network_message(&frame),
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
    frame.extend_from_slice(&(second::CURRENT_NETWORK_PROTOCOL_VERSION + 1).to_be_bytes());
    frame.extend_from_slice(&1_u32.to_be_bytes());
    frame.push(2);

    assert_eq!(
        decode_network_message(&frame),
        Err(NetworkError::UnsupportedProtocolVersion {
            expected: second::CURRENT_NETWORK_PROTOCOL_VERSION,
            actual: second::CURRENT_NETWORK_PROTOCOL_VERSION + 1,
        })
    );
}

#[tokio::test]
async fn two_real_quic_nodes_complete_handshake_and_ping() {
    let (server, certificate) = support::quic_server();
    let address = server.local_addr().unwrap();
    let server_node_id = server.node_id();

    let server_task = tokio::spawn(async move {
        let peer = server.accept().await.unwrap();
        serve_ping_session(&peer).await.unwrap()
    });

    let client = support::quic_client(&certificate);
    let client_node_id = client.node_id();
    let peer = client.connect(address).await.unwrap();

    let remote = client_ping(&peer, 99).await.unwrap();
    assert_eq!(remote, server_node_id);

    peer.close();
    assert_eq!(server_task.await.unwrap(), client_node_id);
}
