use std::collections::BTreeSet;

use crate::validator_registry::ValidatorRegistryRecord;
use crate::{
    PersistenceError, ValidatorCredential, ValidatorId, ValidatorRegistry, ValidatorStatus,
};

use super::codec::{Decoder, push_len};

pub(super) fn encode_validator_registry(
    out: &mut Vec<u8>,
    registry: &ValidatorRegistry,
) -> Result<(), PersistenceError> {
    out.extend_from_slice(&registry.active_validator_set_version().to_be_bytes());
    push_len(out, registry.records().count())?;

    for (validator_id, record) in registry.records() {
        out.extend_from_slice(&validator_id.value().to_be_bytes());
        out.push(match record.status {
            ValidatorStatus::Active => 1,
            ValidatorStatus::Retired => 2,
        });
        out.extend_from_slice(&record.credential.identity_public_key());
        out.extend_from_slice(&record.credential.recovery_public_key());
        out.extend_from_slice(&record.credential.consensus_public_key());

        push_len(out, record.consensus_key_history.len())?;
        for key in &record.consensus_key_history {
            out.extend_from_slice(key);
        }
    }

    Ok(())
}

pub(super) fn decode_validator_registry(
    decoder: &mut Decoder<'_>,
) -> Result<ValidatorRegistry, PersistenceError> {
    let active_validator_set_version = decoder.read_u64()?;
    let record_count = decoder.read_len()?;
    const MIN_RECORD_SIZE: usize = 8 + 1 + 32 * 3 + 8 + 32;
    if record_count > decoder.remaining() / MIN_RECORD_SIZE {
        return Err(PersistenceError::InvalidSnapshot);
    }

    let mut records = Vec::with_capacity(record_count);
    for _ in 0..record_count {
        let validator_id = ValidatorId::new(decoder.read_u64()?);
        let status = match decoder.read_u8()? {
            1 => ValidatorStatus::Active,
            2 => ValidatorStatus::Retired,
            _ => return Err(PersistenceError::InvalidSnapshot),
        };
        let identity_public_key = decoder.read_array_32()?;
        let recovery_public_key = decoder.read_array_32()?;
        let consensus_public_key = decoder.read_array_32()?;
        let history_count = decoder.read_len()?;
        if history_count > decoder.remaining() / 32 {
            return Err(PersistenceError::InvalidSnapshot);
        }

        let mut consensus_key_history = BTreeSet::new();
        for _ in 0..history_count {
            if !consensus_key_history.insert(decoder.read_array_32()?) {
                return Err(PersistenceError::InvalidSnapshot);
            }
        }

        let credential = ValidatorCredential::new(
            validator_id,
            identity_public_key,
            consensus_public_key,
            recovery_public_key,
        )
        .map_err(|_| PersistenceError::InvalidSnapshot)?;

        records.push(ValidatorRegistryRecord {
            credential,
            status,
            consensus_key_history,
        });
    }

    ValidatorRegistry::from_records(active_validator_set_version, records)
        .map_err(|_| PersistenceError::InvalidSnapshot)
}
