use std::collections::HashSet;

use super::{CachedPeer, MAX_LOCAL_PEER_CANDIDATES, NetworkError, peer_store_error};
use crate::ValidatorId;
use crate::network::{NetworkMessage, decode_network_message, encode_network_message};

// Each entry reuses the authoritative PeerRecord wire codec. Role hints stay local.
const HEADER: &[u8; 8] = b"S2PC\0\0\0\x01";
pub(super) const MAX_STORE_SIZE: usize = 8 + MAX_LOCAL_PEER_CANDIDATES as usize * 1200;

pub(super) fn encode(records: &[CachedPeer]) -> Result<Vec<u8>, NetworkError> {
    let mut bytes = HEADER.to_vec();
    for entry in records {
        let frame = encode_network_message(&NetworkMessage::Peers {
            records: vec![entry.record.clone()],
        })?;
        bytes.push(u8::from(entry.validator.is_some()));
        bytes.extend_from_slice(&entry.validator.map_or(0, ValidatorId::value).to_be_bytes());
        bytes.extend_from_slice(&(frame.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&frame);
    }
    Ok(bytes)
}

pub(super) fn decode(bytes: &[u8]) -> Result<Vec<CachedPeer>, NetworkError> {
    let invalid = || peer_store_error("invalid local peer cache");
    if bytes.len() > MAX_STORE_SIZE || !bytes.starts_with(HEADER) {
        return Err(invalid());
    }
    let mut remaining = &bytes[HEADER.len()..];
    let mut records = Vec::new();
    let mut seen = HashSet::new();
    let mut validators = HashSet::new();
    while !remaining.is_empty() {
        if remaining.len() < 13 || records.len() == usize::from(MAX_LOCAL_PEER_CANDIDATES) {
            return Err(invalid());
        }
        let id = u64::from_be_bytes(remaining[1..9].try_into().unwrap());
        let validator = match (remaining[0], id) {
            (0, 0) => None,
            (1, id) => Some(ValidatorId::new(id)),
            _ => return Err(invalid()),
        };
        if validator.is_some_and(|id| !validators.insert(id)) {
            return Err(invalid());
        }
        let length = u32::from_be_bytes(remaining[9..13].try_into().unwrap()) as usize;
        remaining = &remaining[13..];
        let frame = remaining.get(..length).ok_or_else(invalid)?;
        let NetworkMessage::Peers { records: mut page } = decode_network_message(frame)? else {
            return Err(invalid());
        };
        if page.len() != 1 {
            return Err(invalid());
        }
        let record = page.remove(0);
        if !seen.insert(record.node_id()) {
            return Err(invalid());
        }
        records.push(CachedPeer { record, validator });
        remaining = &remaining[length..];
    }
    Ok(records)
}
