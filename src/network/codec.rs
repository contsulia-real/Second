use std::io::{Read, Write};

use crate::{CurrencyAddress, CurrencyRole, PublicCurrencyState, PublicCurrencySummary};

use super::{
    CURRENT_NETWORK_PROTOCOL_VERSION, MAX_NETWORK_FRAME_SIZE, MAX_PUBLIC_CURRENCY_PAGE,
    NetworkError, NetworkMessage, NodeId, validate_public_currency_limit,
};

const NETWORK_MAGIC: [u8; 4] = *b"SCND";
const FRAME_HEADER_SIZE: usize = 12;
const PUBLIC_CURRENCY_ENCODED_SIZE: usize = 11;

pub fn write_network_message<W: Write>(
    writer: &mut W,
    message: &NetworkMessage,
) -> Result<(), NetworkError> {
    let payload = encode_message_payload(message)?;

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
    writer.write_all(&CURRENT_NETWORK_PROTOCOL_VERSION.to_be_bytes())?;
    writer.write_all(&payload_len.to_be_bytes())?;
    writer.write_all(&payload)?;
    writer.flush()?;

    Ok(())
}

pub fn read_network_message<R: Read>(reader: &mut R) -> Result<NetworkMessage, NetworkError> {
    read_network_message_optional(reader)?
        .ok_or(NetworkError::Io(std::io::ErrorKind::UnexpectedEof))
}

pub(crate) fn read_network_message_optional<R: Read>(
    reader: &mut R,
) -> Result<Option<NetworkMessage>, NetworkError> {
    let mut header = [0_u8; FRAME_HEADER_SIZE];

    match reader.read_exact(&mut header[..1]) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(NetworkError::from(error)),
    }

    reader.read_exact(&mut header[1..])?;

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

    if protocol_version != CURRENT_NETWORK_PROTOCOL_VERSION {
        return Err(NetworkError::UnsupportedProtocolVersion {
            expected: CURRENT_NETWORK_PROTOCOL_VERSION,
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

    decode_message_payload(&payload).map(Some)
}

fn encode_message_payload(message: &NetworkMessage) -> Result<Vec<u8>, NetworkError> {
    match message {
        NetworkMessage::Hello { node_id } => {
            let mut payload = Vec::with_capacity(33);
            payload.push(1);
            payload.extend_from_slice(&node_id.to_bytes());
            Ok(payload)
        }
        NetworkMessage::Ping { nonce } => {
            let mut payload = Vec::with_capacity(9);
            payload.push(2);
            payload.extend_from_slice(&nonce.to_be_bytes());
            Ok(payload)
        }
        NetworkMessage::Pong { nonce } => {
            let mut payload = Vec::with_capacity(9);
            payload.push(3);
            payload.extend_from_slice(&nonce.to_be_bytes());
            Ok(payload)
        }
        NetworkMessage::GetPublicCurrencies { start, limit } => {
            validate_public_currency_limit(*limit)?;
            let mut payload = Vec::with_capacity(11);
            payload.push(4);
            payload.extend_from_slice(&start.value().to_be_bytes());
            payload.extend_from_slice(&limit.to_be_bytes());
            Ok(payload)
        }
        NetworkMessage::PublicCurrencies { states, next_start } => {
            if states.len() > usize::from(MAX_PUBLIC_CURRENCY_PAGE) {
                return Err(NetworkError::TooManyPublicCurrencyStates {
                    announced: states.len(),
                    maximum: usize::from(MAX_PUBLIC_CURRENCY_PAGE),
                });
            }

            let count = u16::try_from(states.len()).map_err(|_| {
                NetworkError::TooManyPublicCurrencyStates {
                    announced: states.len(),
                    maximum: usize::from(MAX_PUBLIC_CURRENCY_PAGE),
                }
            })?;

            let mut payload =
                Vec::with_capacity(1 + 2 + states.len() * PUBLIC_CURRENCY_ENCODED_SIZE + 1 + 8);
            payload.push(5);
            payload.extend_from_slice(&count.to_be_bytes());

            for state in states {
                payload.extend_from_slice(&state.address.value().to_be_bytes());
                payload.push(u8::from(state.exists));
                payload.push(u8::from(state.occupied));
                payload.push(match state.role {
                    CurrencyRole::Circulation => 1,
                    CurrencyRole::Reserve => 2,
                });
            }

            match next_start {
                Some(address) => {
                    payload.push(1);
                    payload.extend_from_slice(&address.value().to_be_bytes());
                }
                None => payload.push(0),
            }

            Ok(payload)
        }
        NetworkMessage::GetPublicCurrencySummary => Ok(vec![6]),
        NetworkMessage::PublicCurrencySummary { summary } => {
            let mut payload = Vec::with_capacity(65);
            payload.push(7);
            payload.extend_from_slice(&summary.next_currency_address.to_be_bytes());
            payload.extend_from_slice(&summary.current_supply.to_be_bytes());
            payload.extend_from_slice(&summary.reserve_count.to_be_bytes());
            payload.extend_from_slice(&summary.occupied_count.to_be_bytes());
            payload.extend_from_slice(&summary.state_digest);
            Ok(payload)
        }
    }
}

fn decode_message_payload(payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    let message_type = *payload.first().ok_or(NetworkError::EmptyPayload)?;

    match message_type {
        1 => decode_hello(payload),
        2 => decode_nonce_message(payload, false),
        3 => decode_nonce_message(payload, true),
        4 => decode_public_currency_query(payload),
        5 => decode_public_currency_page(payload),
        6 => {
            require_message_length(6, payload, 1)?;
            Ok(NetworkMessage::GetPublicCurrencySummary)
        }
        7 => decode_public_currency_summary(payload),
        other => Err(NetworkError::UnknownMessageType(other)),
    }
}

fn decode_public_currency_summary(payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    require_message_length(7, payload, 65)?;

    let next_currency_address = u64::from_be_bytes(payload[1..9].try_into().map_err(|_| {
        NetworkError::InvalidMessageLength {
            message_type: 7,
            expected: 65,
            actual: payload.len(),
        }
    })?);
    let current_supply = u64::from_be_bytes(payload[9..17].try_into().map_err(|_| {
        NetworkError::InvalidMessageLength {
            message_type: 7,
            expected: 65,
            actual: payload.len(),
        }
    })?);
    let reserve_count = u64::from_be_bytes(payload[17..25].try_into().map_err(|_| {
        NetworkError::InvalidMessageLength {
            message_type: 7,
            expected: 65,
            actual: payload.len(),
        }
    })?);
    let occupied_count = u64::from_be_bytes(payload[25..33].try_into().map_err(|_| {
        NetworkError::InvalidMessageLength {
            message_type: 7,
            expected: 65,
            actual: payload.len(),
        }
    })?);
    let state_digest =
        payload[33..65]
            .try_into()
            .map_err(|_| NetworkError::InvalidMessageLength {
                message_type: 7,
                expected: 65,
                actual: payload.len(),
            })?;

    Ok(NetworkMessage::PublicCurrencySummary {
        summary: PublicCurrencySummary {
            next_currency_address,
            current_supply,
            reserve_count,
            occupied_count,
            state_digest,
        },
    })
}

fn decode_hello(payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    require_message_length(1, payload, 33)?;
    let mut node_id = [0_u8; 32];
    node_id.copy_from_slice(&payload[1..33]);
    Ok(NetworkMessage::Hello {
        node_id: NodeId::from_bytes(node_id),
    })
}

fn decode_nonce_message(payload: &[u8], pong: bool) -> Result<NetworkMessage, NetworkError> {
    let message_type = if pong { 3 } else { 2 };
    require_message_length(message_type, payload, 9)?;
    let nonce = u64::from_be_bytes(payload[1..9].try_into().map_err(|_| {
        NetworkError::InvalidMessageLength {
            message_type,
            expected: 9,
            actual: payload.len(),
        }
    })?);

    if pong {
        Ok(NetworkMessage::Pong { nonce })
    } else {
        Ok(NetworkMessage::Ping { nonce })
    }
}

fn decode_public_currency_query(payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    require_message_length(4, payload, 11)?;
    let start = CurrencyAddress::new(u64::from_be_bytes(payload[1..9].try_into().map_err(
        |_| NetworkError::InvalidMessageLength {
            message_type: 4,
            expected: 11,
            actual: payload.len(),
        },
    )?));
    let limit = u16::from_be_bytes(payload[9..11].try_into().map_err(|_| {
        NetworkError::InvalidMessageLength {
            message_type: 4,
            expected: 11,
            actual: payload.len(),
        }
    })?);
    validate_public_currency_limit(limit)?;

    Ok(NetworkMessage::GetPublicCurrencies { start, limit })
}

fn decode_public_currency_page(payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    if payload.len() < 4 {
        return Err(NetworkError::InvalidMessageLength {
            message_type: 5,
            expected: 4,
            actual: payload.len(),
        });
    }

    let count = usize::from(u16::from_be_bytes(payload[1..3].try_into().map_err(
        |_| NetworkError::InvalidMessageLength {
            message_type: 5,
            expected: 4,
            actual: payload.len(),
        },
    )?));

    if count > usize::from(MAX_PUBLIC_CURRENCY_PAGE) {
        return Err(NetworkError::TooManyPublicCurrencyStates {
            announced: count,
            maximum: usize::from(MAX_PUBLIC_CURRENCY_PAGE),
        });
    }

    let base_len = 1_usize
        .checked_add(2)
        .and_then(|len| len.checked_add(count * PUBLIC_CURRENCY_ENCODED_SIZE))
        .and_then(|len| len.checked_add(1))
        .ok_or(NetworkError::FrameTooLarge {
            announced: payload.len(),
            maximum: MAX_NETWORK_FRAME_SIZE,
        })?;

    if payload.len() < base_len {
        return Err(NetworkError::InvalidMessageLength {
            message_type: 5,
            expected: base_len,
            actual: payload.len(),
        });
    }

    let mut states = Vec::with_capacity(count);
    let mut offset = 3;

    for _ in 0..count {
        let address = CurrencyAddress::new(u64::from_be_bytes(
            payload[offset..offset + 8].try_into().map_err(|_| {
                NetworkError::InvalidMessageLength {
                    message_type: 5,
                    expected: base_len,
                    actual: payload.len(),
                }
            })?,
        ));
        offset += 8;

        let exists = decode_bool(payload[offset])?;
        offset += 1;
        let occupied = decode_bool(payload[offset])?;
        offset += 1;
        let role = match payload[offset] {
            1 => CurrencyRole::Circulation,
            2 => CurrencyRole::Reserve,
            other => return Err(NetworkError::InvalidCurrencyRole(other)),
        };
        offset += 1;

        states.push(PublicCurrencyState {
            address,
            exists,
            occupied,
            role,
        });
    }

    let cursor_flag = payload[offset];
    offset += 1;

    let next_start = match cursor_flag {
        0 => None,
        1 => {
            let expected = base_len + 8;
            if payload.len() != expected {
                return Err(NetworkError::InvalidMessageLength {
                    message_type: 5,
                    expected,
                    actual: payload.len(),
                });
            }

            Some(CurrencyAddress::new(u64::from_be_bytes(
                payload[offset..offset + 8].try_into().map_err(|_| {
                    NetworkError::InvalidMessageLength {
                        message_type: 5,
                        expected,
                        actual: payload.len(),
                    }
                })?,
            )))
        }
        other => return Err(NetworkError::InvalidCursorFlag(other)),
    };

    if cursor_flag == 0 && payload.len() != base_len {
        return Err(NetworkError::InvalidMessageLength {
            message_type: 5,
            expected: base_len,
            actual: payload.len(),
        });
    }

    Ok(NetworkMessage::PublicCurrencies { states, next_start })
}

fn decode_bool(value: u8) -> Result<bool, NetworkError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        other => Err(NetworkError::InvalidBoolean(other)),
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
