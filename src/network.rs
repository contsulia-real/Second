use std::fmt;
use std::io::{self, Read, Write};

use crate::CURRENT_PROTOCOL_VERSION;

const NETWORK_MAGIC: [u8; 4] = *b"SCND";
const FRAME_HEADER_SIZE: usize = 12;

pub const MAX_NETWORK_FRAME_SIZE: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct NodeId([u8; 32]);

impl NodeId {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn from_u64(value: u64) -> Self {
        let mut bytes = [0_u8; 32];
        bytes[24..].copy_from_slice(&value.to_be_bytes());
        Self(bytes)
    }

    pub const fn to_bytes(self) -> [u8; 32] {
        self.0
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NetworkMessage {
    Hello { node_id: NodeId },
    Ping { nonce: u64 },
    Pong { nonce: u64 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NetworkError {
    Io(io::ErrorKind),
    InvalidMagic,
    UnsupportedProtocolVersion {
        expected: u32,
        actual: u32,
    },
    FrameTooLarge {
        announced: usize,
        maximum: usize,
    },
    EmptyPayload,
    UnknownMessageType(u8),
    InvalidMessageLength {
        message_type: u8,
        expected: usize,
        actual: usize,
    },
    UnexpectedMessage,
    NonceMismatch {
        expected: u64,
        actual: u64,
    },
}

impl From<io::Error> for NetworkError {
    fn from(value: io::Error) -> Self {
        Self::Io(value.kind())
    }
}

pub fn write_network_message<W: Write>(
    writer: &mut W,
    message: &NetworkMessage,
) -> Result<(), NetworkError> {
    let payload = encode_message_payload(message);

    if payload.len() > MAX_NETWORK_FRAME_SIZE {
        return Err(NetworkError::FrameTooLarge {
            announced: payload.len(),
            maximum: MAX_NETWORK_FRAME_SIZE,
        });
    }

    let payload_len = u32::try_from(payload.len()).map_err(|_| NetworkError::FrameTooLarge {
        announced: payload.len(),
        maximum: MAX_NETWORK_FRAME_SIZE,
    })?;

    writer.write_all(&NETWORK_MAGIC)?;
    writer.write_all(&CURRENT_PROTOCOL_VERSION.to_be_bytes())?;
    writer.write_all(&payload_len.to_be_bytes())?;
    writer.write_all(&payload)?;
    writer.flush()?;

    Ok(())
}

pub fn read_network_message<R: Read>(reader: &mut R) -> Result<NetworkMessage, NetworkError> {
    let mut header = [0_u8; FRAME_HEADER_SIZE];
    reader.read_exact(&mut header)?;

    if header[0..4] != NETWORK_MAGIC {
        return Err(NetworkError::InvalidMagic);
    }

    let protocol_version = u32::from_be_bytes(header[4..8].try_into().map_err(|_| {
        NetworkError::InvalidMessageLength {
            message_type: 0,
            expected: FRAME_HEADER_SIZE,
            actual: header.len(),
        }
    })?);

    if protocol_version != CURRENT_PROTOCOL_VERSION {
        return Err(NetworkError::UnsupportedProtocolVersion {
            expected: CURRENT_PROTOCOL_VERSION,
            actual: protocol_version,
        });
    }

    let announced = u32::from_be_bytes(header[8..12].try_into().map_err(|_| {
        NetworkError::InvalidMessageLength {
            message_type: 0,
            expected: FRAME_HEADER_SIZE,
            actual: header.len(),
        }
    })?) as usize;

    if announced > MAX_NETWORK_FRAME_SIZE {
        return Err(NetworkError::FrameTooLarge {
            announced,
            maximum: MAX_NETWORK_FRAME_SIZE,
        });
    }

    let mut payload = vec![0_u8; announced];
    reader.read_exact(&mut payload)?;

    decode_message_payload(&payload)
}

pub fn client_ping<S: Read + Write>(
    stream: &mut S,
    local_node_id: NodeId,
    nonce: u64,
) -> Result<NodeId, NetworkError> {
    write_network_message(
        stream,
        &NetworkMessage::Hello {
            node_id: local_node_id,
        },
    )?;

    let remote_node_id = match read_network_message(stream)? {
        NetworkMessage::Hello { node_id } => node_id,
        _ => return Err(NetworkError::UnexpectedMessage),
    };

    write_network_message(stream, &NetworkMessage::Ping { nonce })?;

    match read_network_message(stream)? {
        NetworkMessage::Pong {
            nonce: response_nonce,
        } if response_nonce == nonce => Ok(remote_node_id),
        NetworkMessage::Pong {
            nonce: response_nonce,
        } => Err(NetworkError::NonceMismatch {
            expected: nonce,
            actual: response_nonce,
        }),
        _ => Err(NetworkError::UnexpectedMessage),
    }
}

pub fn serve_ping_session<S: Read + Write>(
    stream: &mut S,
    local_node_id: NodeId,
) -> Result<NodeId, NetworkError> {
    let remote_node_id = match read_network_message(stream)? {
        NetworkMessage::Hello { node_id } => node_id,
        _ => return Err(NetworkError::UnexpectedMessage),
    };

    write_network_message(
        stream,
        &NetworkMessage::Hello {
            node_id: local_node_id,
        },
    )?;

    match read_network_message(stream)? {
        NetworkMessage::Ping { nonce } => {
            write_network_message(stream, &NetworkMessage::Pong { nonce })?;
            Ok(remote_node_id)
        }
        _ => Err(NetworkError::UnexpectedMessage),
    }
}

fn encode_message_payload(message: &NetworkMessage) -> Vec<u8> {
    match message {
        NetworkMessage::Hello { node_id } => {
            let mut payload = Vec::with_capacity(33);
            payload.push(1);
            payload.extend_from_slice(&node_id.to_bytes());
            payload
        }
        NetworkMessage::Ping { nonce } => {
            let mut payload = Vec::with_capacity(9);
            payload.push(2);
            payload.extend_from_slice(&nonce.to_be_bytes());
            payload
        }
        NetworkMessage::Pong { nonce } => {
            let mut payload = Vec::with_capacity(9);
            payload.push(3);
            payload.extend_from_slice(&nonce.to_be_bytes());
            payload
        }
    }
}

fn decode_message_payload(payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    let message_type = *payload.first().ok_or(NetworkError::EmptyPayload)?;

    match message_type {
        1 => {
            require_message_length(message_type, payload, 33)?;
            let mut node_id = [0_u8; 32];
            node_id.copy_from_slice(&payload[1..33]);
            Ok(NetworkMessage::Hello {
                node_id: NodeId::from_bytes(node_id),
            })
        }
        2 => {
            require_message_length(message_type, payload, 9)?;
            let nonce = u64::from_be_bytes(payload[1..9].try_into().map_err(|_| {
                NetworkError::InvalidMessageLength {
                    message_type,
                    expected: 9,
                    actual: payload.len(),
                }
            })?);
            Ok(NetworkMessage::Ping { nonce })
        }
        3 => {
            require_message_length(message_type, payload, 9)?;
            let nonce = u64::from_be_bytes(payload[1..9].try_into().map_err(|_| {
                NetworkError::InvalidMessageLength {
                    message_type,
                    expected: 9,
                    actual: payload.len(),
                }
            })?);
            Ok(NetworkMessage::Pong { nonce })
        }
        other => Err(NetworkError::UnknownMessageType(other)),
    }
}

fn require_message_length(
    message_type: u8,
    payload: &[u8],
    expected: usize,
) -> Result<(), NetworkError> {
    if payload.len() == expected {
        Ok(())
    } else {
        Err(NetworkError::InvalidMessageLength {
            message_type,
            expected,
            actual: payload.len(),
        })
    }
}
