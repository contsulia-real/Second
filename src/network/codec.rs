use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use crate::{
    CurrencyAddress, CurrencyRole, PublicCurrencyCheckpointProof, PublicCurrencyState,
    PublicCurrencySummary, StateRecoveryCheckpointProof, ValidatorId,
    public_checkpoint::CheckpointProofCodecError,
    state_recovery_checkpoint::StateRecoveryProofCodecError,
};

use super::{
    CURRENT_NETWORK_PROTOCOL_VERSION, MAX_NETWORK_FRAME_SIZE, MAX_PEER_CERTIFICATE_SIZE,
    MAX_PEER_RECORDS, MAX_PUBLIC_CURRENCY_PAGE, MAX_STATE_RECOVERY_CHUNK_SIZE, NetworkError,
    NetworkMessage, NodeId, PeerRecord, validate_chunk_limit, validate_peer_limit,
    validate_public_currency_limit,
};

const NETWORK_MAGIC: [u8; 4] = *b"SCND";
const FRAME_HEADER_SIZE: usize = 12;
pub(super) const MAX_NETWORK_MESSAGE_SIZE: usize = MAX_NETWORK_FRAME_SIZE + FRAME_HEADER_SIZE;
const PUBLIC_CURRENCY_ENCODED_SIZE: usize = 10;

pub fn encode_network_message(message: &NetworkMessage) -> Result<Vec<u8>, NetworkError> {
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

    let mut frame = Vec::with_capacity(FRAME_HEADER_SIZE + payload.len());
    frame.extend_from_slice(&NETWORK_MAGIC);
    frame.extend_from_slice(&CURRENT_NETWORK_PROTOCOL_VERSION.to_be_bytes());
    frame.extend_from_slice(&payload_len.to_be_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

pub(super) fn network_frame_size(frame_prefix: &[u8]) -> Result<usize, NetworkError> {
    if frame_prefix.len() < FRAME_HEADER_SIZE {
        return Err(NetworkError::InvalidMessageLength {
            message_type: 0,
            expected: FRAME_HEADER_SIZE,
            actual: frame_prefix.len(),
        });
    }

    if frame_prefix[0..4] != NETWORK_MAGIC {
        return Err(NetworkError::InvalidMagic);
    }

    let protocol_version = u32::from_be_bytes(frame_prefix[4..8].try_into().map_err(|_| {
        NetworkError::InvalidMessageLength {
            message_type: 0,
            expected: FRAME_HEADER_SIZE,
            actual: frame_prefix.len(),
        }
    })?);

    if protocol_version != CURRENT_NETWORK_PROTOCOL_VERSION {
        return Err(NetworkError::UnsupportedProtocolVersion {
            expected: CURRENT_NETWORK_PROTOCOL_VERSION,
            actual: protocol_version,
        });
    }

    let announced = u32::from_be_bytes(frame_prefix[8..12].try_into().map_err(|_| {
        NetworkError::InvalidMessageLength {
            message_type: 0,
            expected: FRAME_HEADER_SIZE,
            actual: frame_prefix.len(),
        }
    })?) as usize;

    if announced > MAX_NETWORK_FRAME_SIZE {
        return Err(NetworkError::FrameTooLarge {
            announced,
            maximum: MAX_NETWORK_FRAME_SIZE,
        });
    }

    FRAME_HEADER_SIZE
        .checked_add(announced)
        .ok_or(NetworkError::FrameTooLarge {
            announced,
            maximum: MAX_NETWORK_FRAME_SIZE,
        })
}

pub fn decode_network_message(frame: &[u8]) -> Result<NetworkMessage, NetworkError> {
    let expected_len = network_frame_size(frame)?;
    if frame.len() != expected_len {
        return Err(NetworkError::InvalidMessageLength {
            message_type: 0,
            expected: expected_len,
            actual: frame.len(),
        });
    }

    decode_message_payload(&frame[FRAME_HEADER_SIZE..])
}

fn encode_message_payload(message: &NetworkMessage) -> Result<Vec<u8>, NetworkError> {
    match message {
        NetworkMessage::Hello { node_id, signature } => {
            let mut payload = Vec::with_capacity(97);
            payload.push(1);
            payload.extend_from_slice(&node_id.to_bytes());
            payload.extend_from_slice(signature);
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
        NetworkMessage::GetStateRecoveryManifest {
            validator_id,
            signature,
        } => {
            let mut payload = Vec::with_capacity(73);
            payload.push(13);
            payload.extend_from_slice(&validator_id.value().to_be_bytes());
            payload.extend_from_slice(signature);
            Ok(payload)
        }
        NetworkMessage::StateRecoveryManifest { proof, payload_len } => {
            let proof = proof
                .encode_bytes()
                .map_err(|_| NetworkError::InvalidStateRecoveryProof)?;
            let mut payload = Vec::with_capacity(9 + proof.len());
            payload.push(14);
            payload.extend_from_slice(&payload_len.to_be_bytes());
            payload.extend_from_slice(&proof);
            Ok(payload)
        }
        NetworkMessage::NoStateRecoveryCheckpoint => Ok(vec![15]),
        NetworkMessage::GetStateRecoveryChunk {
            validator_id,
            checkpoint_digest,
            offset,
            limit,
            signature,
        } => {
            validate_chunk_limit(*limit)?;
            let mut payload = Vec::with_capacity(117);
            payload.push(16);
            payload.extend_from_slice(&validator_id.value().to_be_bytes());
            payload.extend_from_slice(checkpoint_digest);
            payload.extend_from_slice(&offset.to_be_bytes());
            payload.extend_from_slice(&limit.to_be_bytes());
            payload.extend_from_slice(signature);
            Ok(payload)
        }
        NetworkMessage::StateRecoveryChunk {
            checkpoint_digest,
            offset,
            bytes,
        } => {
            if bytes.is_empty() || bytes.len() > MAX_STATE_RECOVERY_CHUNK_SIZE as usize {
                return Err(NetworkError::InvalidStateRecoveryChunk);
            }
            let len =
                u32::try_from(bytes.len()).map_err(|_| NetworkError::InvalidStateRecoveryChunk)?;
            let mut payload = Vec::with_capacity(45 + bytes.len());
            payload.push(17);
            payload.extend_from_slice(checkpoint_digest);
            payload.extend_from_slice(&offset.to_be_bytes());
            payload.extend_from_slice(&len.to_be_bytes());
            payload.extend_from_slice(bytes);
            Ok(payload)
        }
        NetworkMessage::StateRecoveryDenied => Ok(vec![18]),
        NetworkMessage::BftAuthenticate {
            validator_id,
            signature,
        } => {
            let mut payload = Vec::with_capacity(73);
            payload.push(19);
            payload.extend_from_slice(&validator_id.value().to_be_bytes());
            payload.extend_from_slice(signature);
            Ok(payload)
        }
        NetworkMessage::BftAuthenticated {
            validator_id,
            signature,
        } => {
            let mut payload = Vec::with_capacity(73);
            payload.push(20);
            payload.extend_from_slice(&validator_id.value().to_be_bytes());
            payload.extend_from_slice(signature);
            Ok(payload)
        }
        NetworkMessage::BftMessage { bytes } => {
            if bytes.is_empty() {
                return Err(NetworkError::InvalidBftMessage);
            }
            let mut payload = Vec::with_capacity(1 + bytes.len());
            payload.push(21);
            payload.extend_from_slice(bytes);
            Ok(payload)
        }
        NetworkMessage::BftDenied => Ok(vec![22]),
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
        13 => decode_state_recovery_manifest_request(payload),
        14 => decode_state_recovery_manifest(payload),
        15 => {
            require_message_length(15, payload, 1)?;
            Ok(NetworkMessage::NoStateRecoveryCheckpoint)
        }
        16 => decode_state_recovery_chunk_request(payload),
        17 => decode_state_recovery_chunk(payload),
        18 => {
            require_message_length(18, payload, 1)?;
            Ok(NetworkMessage::StateRecoveryDenied)
        }
        19 => decode_bft_authenticate(payload),
        20 => decode_bft_authenticated(payload),
        21 => {
            if payload.len() <= 1 {
                return Err(NetworkError::InvalidBftMessage);
            }
            Ok(NetworkMessage::BftMessage {
                bytes: payload[1..].to_vec(),
            })
        }
        22 => {
            require_message_length(22, payload, 1)?;
            Ok(NetworkMessage::BftDenied)
        }
        other => Err(NetworkError::UnknownMessageType(other)),
    }
}

fn decode_bft_authenticate(payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    require_message_length(19, payload, 73)?;
    let validator_id = ValidatorId::new(u64::from_be_bytes(
        payload[1..9]
            .try_into()
            .map_err(|_| NetworkError::InvalidBftMessage)?,
    ));
    let signature = payload[9..73]
        .try_into()
        .map_err(|_| NetworkError::InvalidBftMessage)?;
    Ok(NetworkMessage::BftAuthenticate {
        validator_id,
        signature,
    })
}

fn decode_bft_authenticated(payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    require_message_length(20, payload, 73)?;
    let validator_id = ValidatorId::new(u64::from_be_bytes(
        payload[1..9]
            .try_into()
            .map_err(|_| NetworkError::InvalidBftMessage)?,
    ));
    let signature = payload[9..73]
        .try_into()
        .map_err(|_| NetworkError::InvalidBftMessage)?;
    Ok(NetworkMessage::BftAuthenticated {
        validator_id,
        signature,
    })
}

fn decode_state_recovery_manifest_request(payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    require_message_length(13, payload, 73)?;
    let validator_id = ValidatorId::new(u64::from_be_bytes(
        payload[1..9]
            .try_into()
            .map_err(|_| NetworkError::InvalidStateRecoveryProof)?,
    ));
    let signature = payload[9..73]
        .try_into()
        .map_err(|_| NetworkError::InvalidStateRecoveryProof)?;
    Ok(NetworkMessage::GetStateRecoveryManifest {
        validator_id,
        signature,
    })
}

fn decode_state_recovery_manifest(payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    if payload.len() <= 9 {
        return Err(NetworkError::InvalidStateRecoveryProof);
    }
    let payload_len = u64::from_be_bytes(
        payload[1..9]
            .try_into()
            .map_err(|_| NetworkError::InvalidStateRecoveryProof)?,
    );
    let proof =
        StateRecoveryCheckpointProof::decode_bytes(&payload[9..]).map_err(|error| match error {
            StateRecoveryProofCodecError::LengthOverflow
            | StateRecoveryProofCodecError::InvalidLength => {
                NetworkError::InvalidStateRecoveryProof
            }
        })?;
    Ok(NetworkMessage::StateRecoveryManifest { proof, payload_len })
}

fn decode_state_recovery_chunk_request(payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    require_message_length(16, payload, 117)?;
    let validator_id = ValidatorId::new(u64::from_be_bytes(
        payload[1..9]
            .try_into()
            .map_err(|_| NetworkError::InvalidStateRecoveryChunk)?,
    ));
    let checkpoint_digest = payload[9..41]
        .try_into()
        .map_err(|_| NetworkError::InvalidStateRecoveryChunk)?;
    let offset = u64::from_be_bytes(
        payload[41..49]
            .try_into()
            .map_err(|_| NetworkError::InvalidStateRecoveryChunk)?,
    );
    let limit = u32::from_be_bytes(
        payload[49..53]
            .try_into()
            .map_err(|_| NetworkError::InvalidStateRecoveryChunk)?,
    );
    validate_chunk_limit(limit)?;
    let signature = payload[53..117]
        .try_into()
        .map_err(|_| NetworkError::InvalidStateRecoveryChunk)?;
    Ok(NetworkMessage::GetStateRecoveryChunk {
        validator_id,
        checkpoint_digest,
        offset,
        limit,
        signature,
    })
}

fn decode_state_recovery_chunk(payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    if payload.len() < 45 {
        return Err(NetworkError::InvalidStateRecoveryChunk);
    }
    let checkpoint_digest = payload[1..33]
        .try_into()
        .map_err(|_| NetworkError::InvalidStateRecoveryChunk)?;
    let offset = u64::from_be_bytes(
        payload[33..41]
            .try_into()
            .map_err(|_| NetworkError::InvalidStateRecoveryChunk)?,
    );
    let len = usize::try_from(u32::from_be_bytes(
        payload[41..45]
            .try_into()
            .map_err(|_| NetworkError::InvalidStateRecoveryChunk)?,
    ))
    .map_err(|_| NetworkError::InvalidStateRecoveryChunk)?;
    if len == 0 || len > MAX_STATE_RECOVERY_CHUNK_SIZE as usize {
        return Err(NetworkError::InvalidStateRecoveryChunk);
    }
    let expected = 45_usize
        .checked_add(len)
        .ok_or(NetworkError::InvalidStateRecoveryChunk)?;
    if payload.len() != expected {
        return Err(NetworkError::InvalidStateRecoveryChunk);
    }
    Ok(NetworkMessage::StateRecoveryChunk {
        checkpoint_digest,
        offset,
        bytes: payload[45..].to_vec(),
    })
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
    require_message_length(1, payload, 97)?;
    let mut node_id = [0_u8; 32];
    node_id.copy_from_slice(&payload[1..33]);
    let mut signature = [0_u8; 64];
    signature.copy_from_slice(&payload[33..97]);
    Ok(NetworkMessage::Hello {
        node_id: NodeId::from_bytes(node_id),
        signature,
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
