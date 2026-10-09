//! Recovery transport envelope. Authority proofs do not change business truth.
use super::codec::{Decoder, push_len};
use crate::{PersistenceError, ValidatorSetTransitionProof};
use std::collections::BTreeMap;

pub(crate) fn encode(
    state: &[u8],
    proofs: &BTreeMap<u64, ValidatorSetTransitionProof>,
) -> Result<Vec<u8>, PersistenceError> {
    if proofs.len() > 1 {
        return Err(PersistenceError::InvalidSnapshot);
    }
    let mut bytes = Vec::with_capacity(state.len() + 16);
    push_len(&mut bytes, state.len())?;
    bytes.extend_from_slice(state);
    push_len(&mut bytes, proofs.len())?;
    for (version, proof) in proofs {
        bytes.extend_from_slice(&version.to_be_bytes());
        let encoded = proof
            .encode_bytes()
            .map_err(|_| PersistenceError::InvalidSnapshot)?;
        push_len(&mut bytes, encoded.len())?;
        bytes.extend_from_slice(&encoded);
    }
    Ok(bytes)
}

pub(crate) fn decode(
    bytes: &[u8],
) -> Result<(&[u8], BTreeMap<u64, ValidatorSetTransitionProof>), PersistenceError> {
    let mut reader = Decoder::new(bytes);
    let length = reader.read_len()?;
    let state = reader.read_exact(length)?;
    let count = reader.read_len()?;
    if count > 1 {
        return Err(PersistenceError::InvalidSnapshot);
    }
    let mut proofs = BTreeMap::new();
    for _ in 0..count {
        let version = reader.read_u64()?;
        let length = reader.read_len()?;
        let proof = ValidatorSetTransitionProof::decode_bytes(reader.read_exact(length)?)
            .map_err(|_| PersistenceError::InvalidSnapshot)?;
        proofs.insert(version, proof);
    }
    reader.finish()?;
    Ok((state, proofs))
}
