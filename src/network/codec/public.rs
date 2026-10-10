use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use super::super::{
    MAX_NETWORK_FRAME_SIZE, MAX_PEER_CERTIFICATE_SIZE, MAX_PEER_RECORDS, MAX_PUBLIC_CURRENCY_PAGE,
    NetworkError, NetworkMessage, NodeId, PeerRecord, validate_peer_limit,
    validate_public_currency_limit,
};
use super::require_message_length;
use crate::{
    CurrencyAddress, PublicCurrencyCheckpointProof, PublicCurrencyDelta,
    ValidatorSetTransitionProof,
    public_checkpoint::CheckpointProofCodecError,
    public_state_codec::{
        PUBLIC_CURRENCY_STATE_ENCODED_SIZE, PUBLIC_CURRENCY_SUMMARY_ENCODED_SIZE,
        PublicStateCodecError, decode_public_currency_state,
        decode_public_currency_summary as decode_public_currency_summary_bytes,
        encode_public_currency_state, encode_public_currency_summary,
    },
};

pub(super) fn encode(message: &NetworkMessage) -> Result<Vec<u8>, NetworkError> {
    match message {
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

            let mut payload = Vec::with_capacity(
                1 + 2 + states.len() * PUBLIC_CURRENCY_STATE_ENCODED_SIZE + 1 + 8,
            );
            payload.push(5);
            payload.extend_from_slice(&count.to_be_bytes());

            for state in states {
                encode_public_currency_state(&mut payload, state);
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
            let mut payload = Vec::with_capacity(1 + PUBLIC_CURRENCY_SUMMARY_ENCODED_SIZE);
            payload.push(7);
            encode_public_currency_summary(&mut payload, summary);
            Ok(payload)
        }
        NetworkMessage::GetPublicCurrencyCheckpoint => Ok(vec![8]),
        NetworkMessage::PublicCurrencyCheckpointProof { proof } => encode_checkpoint_proof(proof),
        NetworkMessage::NoPublicCurrencyCheckpoint => Ok(vec![10]),
        NetworkMessage::GetPeers { limit } => {
            validate_peer_limit(*limit)?;
            let mut payload = Vec::with_capacity(3);
            payload.push(11);
            payload.extend_from_slice(&limit.to_be_bytes());
            Ok(payload)
        }
        NetworkMessage::Peers { records } => encode_peer_records(records),
        NetworkMessage::GetPublicCurrencyDelta {
            from_epoch,
            from_state_digest,
        } => {
            let mut payload = Vec::with_capacity(41);
            payload.push(33);
            payload.extend_from_slice(&from_epoch.to_be_bytes());
            payload.extend_from_slice(from_state_digest);
            Ok(payload)
        }
        NetworkMessage::PublicCurrencyDelta { delta } => {
            let bytes = delta
                .encode_bytes()
                .map_err(|_| NetworkError::InvalidPublicCurrencyDelta)?;
            let mut payload = Vec::with_capacity(1 + bytes.len());
            payload.push(34);
            payload.extend_from_slice(&bytes);
            Ok(payload)
        }
        NetworkMessage::NoPublicCurrencyDelta => Ok(vec![35]),
        NetworkMessage::GetValidatorSetTransitionProof {
            current_validator_set_version,
        } => {
            let mut payload = Vec::with_capacity(9);
            payload.push(36);
            payload.extend_from_slice(&current_validator_set_version.to_be_bytes());
            Ok(payload)
        }
        NetworkMessage::ValidatorSetTransitionProof { proof } => {
            let bytes = proof
                .encode_bytes()
                .map_err(|_| NetworkError::InvalidValidatorTransitionProof)?;
            let mut payload = Vec::with_capacity(1 + bytes.len());
            payload.push(37);
            payload.extend_from_slice(&bytes);
            Ok(payload)
        }
        NetworkMessage::NoValidatorSetTransitionProof => Ok(vec![38]),
        _ => Err(NetworkError::UnexpectedMessage),
    }
}

pub(super) fn decode(message_type: u8, payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    match message_type {
        4 => decode_public_currency_query(payload),
        5 => decode_public_currency_page(payload),
        6 => {
            require_message_length(6, payload, 1)?;
            Ok(NetworkMessage::GetPublicCurrencySummary)
        }
        7 => decode_public_currency_summary(payload),
        8 => {
            require_message_length(8, payload, 1)?;
            Ok(NetworkMessage::GetPublicCurrencyCheckpoint)
        }
        9 => decode_checkpoint_proof(payload),
        10 => {
            require_message_length(10, payload, 1)?;
            Ok(NetworkMessage::NoPublicCurrencyCheckpoint)
        }
        11 => decode_peer_query(payload),
        12 => decode_peer_records(payload),
        33 => decode_public_currency_delta_request(payload),
        34 => decode_public_currency_delta(payload),
        35 => {
            require_message_length(35, payload, 1)?;
            Ok(NetworkMessage::NoPublicCurrencyDelta)
        }
        36 => decode_validator_transition_proof_request(payload),
        37 => decode_validator_transition_proof(payload),
        38 => {
            require_message_length(38, payload, 1)?;
            Ok(NetworkMessage::NoValidatorSetTransitionProof)
        }
        other => Err(NetworkError::UnknownMessageType(other)),
    }
}

fn encode_peer_records(records: &[PeerRecord]) -> Result<Vec<u8>, NetworkError> {
    if records.len() > usize::from(MAX_PEER_RECORDS) {
        return Err(NetworkError::TooManyPeerRecords {
            announced: records.len(),
            maximum: usize::from(MAX_PEER_RECORDS),
        });
    }

    let count = u16::try_from(records.len()).map_err(|_| NetworkError::TooManyPeerRecords {
        announced: records.len(),
        maximum: usize::from(MAX_PEER_RECORDS),
    })?;
    let mut payload = Vec::new();
    payload.push(12);
    payload.extend_from_slice(&count.to_be_bytes());

    for record in records {
        payload.extend_from_slice(&record.node_id().to_bytes());
        match record.address() {
            SocketAddr::V4(address) => {
                payload.push(4);
                payload.extend_from_slice(&address.ip().octets());
                payload.extend_from_slice(&address.port().to_be_bytes());
            }
            SocketAddr::V6(address) => {
                payload.push(6);
                payload.extend_from_slice(&address.ip().octets());
                payload.extend_from_slice(&address.port().to_be_bytes());
            }
        }

        let certificate = record.certificate_der();
        if certificate.is_empty() || certificate.len() > MAX_PEER_CERTIFICATE_SIZE {
            return Err(NetworkError::InvalidPeerRecord);
        }
        let certificate_len =
            u16::try_from(certificate.len()).map_err(|_| NetworkError::InvalidPeerRecord)?;
        payload.extend_from_slice(&certificate_len.to_be_bytes());
        payload.extend_from_slice(certificate);
    }

    Ok(payload)
}

fn decode_public_currency_delta_request(payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    require_message_length(33, payload, 41)?;
    Ok(NetworkMessage::GetPublicCurrencyDelta {
        from_epoch: u64::from_be_bytes(
            payload[1..9]
                .try_into()
                .map_err(|_| NetworkError::InvalidPublicCurrencyDelta)?,
        ),
        from_state_digest: payload[9..41]
            .try_into()
            .map_err(|_| NetworkError::InvalidPublicCurrencyDelta)?,
    })
}

fn decode_public_currency_delta(payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    if payload.len() <= 1 {
        return Err(NetworkError::InvalidPublicCurrencyDelta);
    }
    let delta = PublicCurrencyDelta::decode_bytes(&payload[1..])
        .map_err(|_| NetworkError::InvalidPublicCurrencyDelta)?;
    Ok(NetworkMessage::PublicCurrencyDelta { delta })
}

fn decode_validator_transition_proof_request(
    payload: &[u8],
) -> Result<NetworkMessage, NetworkError> {
    require_message_length(36, payload, 9)?;
    Ok(NetworkMessage::GetValidatorSetTransitionProof {
        current_validator_set_version: u64::from_be_bytes(
            payload[1..9]
                .try_into()
                .map_err(|_| NetworkError::InvalidValidatorTransitionProof)?,
        ),
    })
}

fn decode_validator_transition_proof(payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    if payload.len() <= 1 {
        return Err(NetworkError::InvalidValidatorTransitionProof);
    }
    let proof = ValidatorSetTransitionProof::decode_bytes(&payload[1..])
        .map_err(|_| NetworkError::InvalidValidatorTransitionProof)?;
    Ok(NetworkMessage::ValidatorSetTransitionProof { proof })
}

fn decode_peer_query(payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    require_message_length(11, payload, 3)?;
    let limit = u16::from_be_bytes(
        payload[1..3]
            .try_into()
            .map_err(|_| NetworkError::InvalidPeerRecord)?,
    );
    validate_peer_limit(limit)?;
    Ok(NetworkMessage::GetPeers { limit })
}

fn decode_peer_records(payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    let mut offset = 1;
    let count = usize::from(u16::from_be_bytes(take_peer_bytes::<2>(
        payload,
        &mut offset,
    )?));
    if count > usize::from(MAX_PEER_RECORDS) {
        return Err(NetworkError::TooManyPeerRecords {
            announced: count,
            maximum: usize::from(MAX_PEER_RECORDS),
        });
    }

    let mut records = Vec::with_capacity(count);
    for _ in 0..count {
        let node_id = NodeId::from_bytes(take_peer_bytes::<32>(payload, &mut offset)?);
        let family = take_peer_bytes::<1>(payload, &mut offset)?[0];
        let ip = match family {
            4 => IpAddr::V4(Ipv4Addr::from(take_peer_bytes::<4>(payload, &mut offset)?)),
            6 => IpAddr::V6(Ipv6Addr::from(take_peer_bytes::<16>(payload, &mut offset)?)),
            other => return Err(NetworkError::InvalidPeerAddressFamily(other)),
        };
        let port = u16::from_be_bytes(take_peer_bytes::<2>(payload, &mut offset)?);
        let certificate_len = usize::from(u16::from_be_bytes(take_peer_bytes::<2>(
            payload,
            &mut offset,
        )?));
        if certificate_len == 0 || certificate_len > MAX_PEER_CERTIFICATE_SIZE {
            return Err(NetworkError::InvalidPeerRecord);
        }

        let certificate_end = offset
            .checked_add(certificate_len)
            .ok_or(NetworkError::InvalidPeerRecord)?;
        let certificate = payload
            .get(offset..certificate_end)
            .ok_or(NetworkError::InvalidPeerRecord)?
            .to_vec();
        offset = certificate_end;

        records.push(PeerRecord::new(
            node_id,
            SocketAddr::new(ip, port),
            certificate,
        )?);
    }

    if offset != payload.len() {
        return Err(NetworkError::InvalidPeerRecord);
    }

    Ok(NetworkMessage::Peers { records })
}

fn take_peer_bytes<const N: usize>(
    payload: &[u8],
    offset: &mut usize,
) -> Result<[u8; N], NetworkError> {
    let end = offset
        .checked_add(N)
        .ok_or(NetworkError::InvalidPeerRecord)?;
    let bytes = payload
        .get(*offset..end)
        .ok_or(NetworkError::InvalidPeerRecord)?
        .try_into()
        .map_err(|_| NetworkError::InvalidPeerRecord)?;
    *offset = end;
    Ok(bytes)
}

fn encode_checkpoint_proof(proof: &PublicCurrencyCheckpointProof) -> Result<Vec<u8>, NetworkError> {
    let proof_bytes = proof.encode_bytes().map_err(map_checkpoint_codec_error)?;
    let encoded_len = proof_bytes
        .len()
        .checked_add(1)
        .ok_or(NetworkError::FrameTooLarge {
            announced: usize::MAX,
            maximum: MAX_NETWORK_FRAME_SIZE,
        })?;

    if encoded_len > MAX_NETWORK_FRAME_SIZE {
        return Err(NetworkError::FrameTooLarge {
            announced: encoded_len,
            maximum: MAX_NETWORK_FRAME_SIZE,
        });
    }

    let mut payload = Vec::with_capacity(encoded_len);
    payload.push(9);
    payload.extend_from_slice(&proof_bytes);
    Ok(payload)
}

fn decode_checkpoint_proof(payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    let proof = PublicCurrencyCheckpointProof::decode_bytes(
        payload
            .get(1..)
            .ok_or(NetworkError::InvalidCheckpointProof)?,
    )
    .map_err(map_checkpoint_codec_error)?;

    Ok(NetworkMessage::PublicCurrencyCheckpointProof { proof })
}

fn map_checkpoint_codec_error(error: CheckpointProofCodecError) -> NetworkError {
    match error {
        CheckpointProofCodecError::LengthOverflow => NetworkError::FrameTooLarge {
            announced: usize::MAX,
            maximum: MAX_NETWORK_FRAME_SIZE,
        },
        CheckpointProofCodecError::InvalidLength => NetworkError::InvalidCheckpointProof,
    }
}

fn decode_public_currency_summary(payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    let expected = 1 + PUBLIC_CURRENCY_SUMMARY_ENCODED_SIZE;
    require_message_length(7, payload, expected)?;
    let summary = decode_public_currency_summary_bytes(&payload[1..]).map_err(|_| {
        NetworkError::InvalidMessageLength {
            message_type: 7,
            expected,
            actual: payload.len(),
        }
    })?;
    Ok(NetworkMessage::PublicCurrencySummary { summary })
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
        .and_then(|len| len.checked_add(count * PUBLIC_CURRENCY_STATE_ENCODED_SIZE))
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
        let end = offset + PUBLIC_CURRENCY_STATE_ENCODED_SIZE;
        let state =
            decode_public_currency_state(&payload[offset..end]).map_err(|error| match error {
                PublicStateCodecError::Boolean(value) => NetworkError::InvalidBoolean(value),
                PublicStateCodecError::InvalidRange => NetworkError::InvalidPublicCurrencyPage,
                PublicStateCodecError::Length => NetworkError::InvalidMessageLength {
                    message_type: 5,
                    expected: base_len,
                    actual: payload.len(),
                },
            })?;
        states.push(state);
        offset = end;
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
