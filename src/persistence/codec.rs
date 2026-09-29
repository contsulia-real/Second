use std::collections::{BTreeMap, BTreeSet};

use sha2::{Digest, Sha256};

use crate::currency::Currency;
use crate::state::{BusinessState, ProtocolState, TaskBinding};
use crate::{
    AccountAddress, CurrencyAddress, CurrencyRole, PersistedNodeState, PersistenceError,
    PublicCurrencyCheckpointProof, SecondState, TaskId, ValidatorCredential, ValidatorId,
    ValidatorSet,
};

const SNAPSHOT_MAGIC: [u8; 4] = *b"S2SN";
const SNAPSHOT_VERSION: u32 = 4;
const LEGACY_SNAPSHOT_VERSION_V3: u32 = 3;
const LEGACY_SNAPSHOT_VERSION_V2: u32 = 2;
const SNAPSHOT_DOMAIN_V4: &[u8] = b"SECOND_STATE_SNAPSHOT_V4\0";
const SNAPSHOT_DOMAIN_V3: &[u8] = b"SECOND_STATE_SNAPSHOT_V3\0";
const SNAPSHOT_DOMAIN_V2: &[u8] = b"SECOND_STATE_SNAPSHOT_V2\0";
const CHECKSUM_SIZE: usize = 32;
const HEADER_SIZE: usize = 4 + 4 + 8 + 8;
const MAX_SNAPSHOT_PAYLOAD_SIZE: u64 = 512 * 1024 * 1024;

pub(super) const MAX_SNAPSHOT_FILE_SIZE: u64 =
    MAX_SNAPSHOT_PAYLOAD_SIZE + (HEADER_SIZE + CHECKSUM_SIZE) as u64;

pub(super) fn encode_snapshot(
    generation: u64,
    state: &SecondState,
    validator_set: &ValidatorSet,
    public_checkpoint_proof: Option<&PublicCurrencyCheckpointProof>,
) -> Result<Vec<u8>, PersistenceError> {
    validate_checkpoint_attachment(state, validator_set, public_checkpoint_proof)?;
    let payload = encode_payload(state, validator_set, public_checkpoint_proof)?;
    let payload_len =
        u64::try_from(payload.len()).map_err(|_| PersistenceError::SnapshotTooLarge)?;

    if payload_len > MAX_SNAPSHOT_PAYLOAD_SIZE {
        return Err(PersistenceError::SnapshotTooLarge);
    }

    let mut bytes = Vec::with_capacity(HEADER_SIZE + payload.len() + CHECKSUM_SIZE);
    bytes.extend_from_slice(&SNAPSHOT_MAGIC);
    bytes.extend_from_slice(&SNAPSHOT_VERSION.to_be_bytes());
    bytes.extend_from_slice(&generation.to_be_bytes());
    bytes.extend_from_slice(&payload_len.to_be_bytes());
    bytes.extend_from_slice(&payload);

    let checksum = snapshot_checksum(SNAPSHOT_VERSION, &bytes)?;
    bytes.extend_from_slice(&checksum);
    Ok(bytes)
}

pub(super) fn decode_snapshot(bytes: &[u8]) -> Result<PersistedNodeState, PersistenceError> {
    if bytes.len() < HEADER_SIZE + CHECKSUM_SIZE {
        return Err(PersistenceError::InvalidSnapshot);
    }

    if bytes[0..4] != SNAPSHOT_MAGIC {
        return Err(PersistenceError::InvalidSnapshot);
    }

    let version = u32::from_be_bytes(
        bytes[4..8]
            .try_into()
            .map_err(|_| PersistenceError::InvalidSnapshot)?,
    );
    if version != SNAPSHOT_VERSION
        && version != LEGACY_SNAPSHOT_VERSION_V3
        && version != LEGACY_SNAPSHOT_VERSION_V2
    {
        return Err(PersistenceError::UnsupportedSnapshotVersion(version));
    }

    let generation = u64::from_be_bytes(
        bytes[8..16]
            .try_into()
            .map_err(|_| PersistenceError::InvalidSnapshot)?,
    );
    let payload_len = u64::from_be_bytes(
        bytes[16..24]
            .try_into()
            .map_err(|_| PersistenceError::InvalidSnapshot)?,
    );

    if payload_len > MAX_SNAPSHOT_PAYLOAD_SIZE {
        return Err(PersistenceError::SnapshotTooLarge);
    }

    let payload_len =
        usize::try_from(payload_len).map_err(|_| PersistenceError::SnapshotTooLarge)?;
    let expected_len = HEADER_SIZE
        .checked_add(payload_len)
        .and_then(|len| len.checked_add(CHECKSUM_SIZE))
        .ok_or(PersistenceError::SnapshotTooLarge)?;

    if bytes.len() != expected_len {
        return Err(PersistenceError::InvalidSnapshot);
    }

    let checksum_offset = HEADER_SIZE + payload_len;
    let expected_checksum = snapshot_checksum(version, &bytes[..checksum_offset])?;
    if bytes[checksum_offset..] != expected_checksum {
        return Err(PersistenceError::ChecksumMismatch);
    }

    let (state, validator_set, public_checkpoint_proof) =
        decode_payload(&bytes[HEADER_SIZE..checksum_offset], version)?;
    validate_checkpoint_attachment(&state, &validator_set, public_checkpoint_proof.as_ref())?;

    Ok(PersistedNodeState {
        state,
        validator_set,
        public_checkpoint_proof,
        generation,
    })
}

fn snapshot_checksum(version: u32, bytes: &[u8]) -> Result<[u8; 32], PersistenceError> {
    let domain = match version {
        SNAPSHOT_VERSION => SNAPSHOT_DOMAIN_V4,
        LEGACY_SNAPSHOT_VERSION_V3 => SNAPSHOT_DOMAIN_V3,
        LEGACY_SNAPSHOT_VERSION_V2 => SNAPSHOT_DOMAIN_V2,
        other => return Err(PersistenceError::UnsupportedSnapshotVersion(other)),
    };

    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(bytes);
    Ok(hasher.finalize().into())
}

fn encode_payload(
    state: &SecondState,
    validator_set: &ValidatorSet,
    public_checkpoint_proof: Option<&PublicCurrencyCheckpointProof>,
) -> Result<Vec<u8>, PersistenceError> {
    let mut out = Vec::new();

    out.extend_from_slice(&state.protocol.next_currency_address.to_be_bytes());

    push_len(&mut out, state.business.accounts.len())?;
    for account in &state.business.accounts {
        out.extend_from_slice(&account.value().to_be_bytes());
    }

    push_len(&mut out, state.business.currencies.len())?;
    for currency in state.business.currencies.values() {
        out.extend_from_slice(&currency.address.value().to_be_bytes());
        out.push(match currency.role {
            CurrencyRole::Circulation => 1,
            CurrencyRole::Reserve => 2,
        });
        match currency.owner {
            Some(owner) => {
                out.push(1);
                out.extend_from_slice(&owner.value().to_be_bytes());
            }
            None => out.push(0),
        }
    }

    push_len(&mut out, state.protocol.task_bindings.len())?;
    for (task_id, binding) in &state.protocol.task_bindings {
        out.extend_from_slice(&task_id.value().to_be_bytes());
        out.extend_from_slice(&binding.request_digest);
        out.push(u8::from(binding.succeeded));
    }

    out.extend_from_slice(&validator_set.version().to_be_bytes());
    push_len(&mut out, validator_set.len())?;
    for credential in validator_set.credentials() {
        out.extend_from_slice(&credential.id().value().to_be_bytes());
        out.extend_from_slice(&credential.identity_public_key());
        out.extend_from_slice(&credential.consensus_public_key());
        out.extend_from_slice(&credential.recovery_public_key());
    }

    match public_checkpoint_proof {
        Some(proof) => {
            out.push(1);
            let proof_bytes = proof
                .encode_bytes()
                .map_err(|_| PersistenceError::InvalidSnapshot)?;
            push_len(&mut out, proof_bytes.len())?;
            out.extend_from_slice(&proof_bytes);
        }
        None => out.push(0),
    }

    push_len(&mut out, state.protocol.validator_vote_locks.len())?;
    for ((validator_id, task_id), plan_digest) in &state.protocol.validator_vote_locks {
        out.extend_from_slice(&validator_id.value().to_be_bytes());
        out.extend_from_slice(&task_id.value().to_be_bytes());
        out.extend_from_slice(plan_digest);
    }

    Ok(out)
}

fn decode_payload(
    payload: &[u8],
    snapshot_version: u32,
) -> Result<
    (
        SecondState,
        ValidatorSet,
        Option<PublicCurrencyCheckpointProof>,
    ),
    PersistenceError,
> {
    let mut decoder = Decoder::new(payload);

    let next_currency_address = decoder.read_u64()?;

    let account_count = decoder.read_len()?;
    let mut accounts = BTreeSet::new();
    for _ in 0..account_count {
        let account = AccountAddress::new(decoder.read_u64()?);
        if !accounts.insert(account) {
            return Err(PersistenceError::InvalidSnapshot);
        }
    }

    let currency_count = decoder.read_len()?;
    let mut currencies = BTreeMap::new();
    for _ in 0..currency_count {
        let address = CurrencyAddress::new(decoder.read_u64()?);
        if address.value() >= next_currency_address {
            return Err(PersistenceError::InvalidSnapshot);
        }

        let role = match decoder.read_u8()? {
            1 => CurrencyRole::Circulation,
            2 => CurrencyRole::Reserve,
            _ => return Err(PersistenceError::InvalidSnapshot),
        };

        let owner = match decoder.read_u8()? {
            0 => None,
            1 => {
                let owner = AccountAddress::new(decoder.read_u64()?);
                if !accounts.contains(&owner) {
                    return Err(PersistenceError::InvalidSnapshot);
                }
                Some(owner)
            }
            _ => return Err(PersistenceError::InvalidSnapshot),
        };

        if role == CurrencyRole::Reserve && owner.is_some() {
            return Err(PersistenceError::InvalidSnapshot);
        }

        if currencies
            .insert(
                address,
                Currency {
                    address,
                    role,
                    owner,
                },
            )
            .is_some()
        {
            return Err(PersistenceError::InvalidSnapshot);
        }
    }

    let task_count = decoder.read_len()?;
    let mut task_bindings = BTreeMap::new();
    for _ in 0..task_count {
        let task_id = TaskId::new(decoder.read_u128()?);
        let request_digest = decoder.read_array_32()?;
        let succeeded = match decoder.read_u8()? {
            0 => false,
            1 => true,
            _ => return Err(PersistenceError::InvalidSnapshot),
        };

        if task_bindings
            .insert(
                task_id,
                TaskBinding {
                    request_digest,
                    succeeded,
                },
            )
            .is_some()
        {
            return Err(PersistenceError::InvalidSnapshot);
        }
    }

    let validator_set_version = decoder.read_u64()?;
    let validator_count = decoder.read_len()?;
    let mut validators = Vec::with_capacity(validator_count);

    for _ in 0..validator_count {
        let id = ValidatorId::new(decoder.read_u64()?);
        let identity_public_key = decoder.read_array_32()?;
        let consensus_public_key = decoder.read_array_32()?;
        let recovery_public_key = decoder.read_array_32()?;
        validators.push(
            ValidatorCredential::new(
                id,
                identity_public_key,
                consensus_public_key,
                recovery_public_key,
            )
            .map_err(|_| PersistenceError::InvalidSnapshot)?,
        );
    }

    let public_checkpoint_proof = if snapshot_version == LEGACY_SNAPSHOT_VERSION_V2 {
        None
    } else {
        match decoder.read_u8()? {
            0 => None,
            1 => {
                let proof_len = decoder.read_len()?;
                let proof_bytes = decoder.read_exact(proof_len)?;
                Some(
                    PublicCurrencyCheckpointProof::decode_bytes(proof_bytes)
                        .map_err(|_| PersistenceError::InvalidSnapshot)?,
                )
            }
            _ => return Err(PersistenceError::InvalidSnapshot),
        }
    };

    let validator_vote_locks = if snapshot_version == SNAPSHOT_VERSION {
        let lock_count = decoder.read_len()?;
        let mut locks = BTreeMap::new();

        for _ in 0..lock_count {
            let validator_id = ValidatorId::new(decoder.read_u64()?);
            let task_id = TaskId::new(decoder.read_u128()?);
            let plan_digest = decoder.read_array_32()?;

            if locks.insert((validator_id, task_id), plan_digest).is_some() {
                return Err(PersistenceError::InvalidSnapshot);
            }
        }

        locks
    } else {
        BTreeMap::new()
    };

    decoder.finish()?;

    let validator_set = ValidatorSet::new(validator_set_version, validators)
        .map_err(|_| PersistenceError::InvalidSnapshot)?;

    Ok((
        SecondState {
            protocol: ProtocolState {
                next_currency_address,
                task_bindings,
                validator_vote_locks,
            },
            business: BusinessState {
                accounts,
                currencies,
            },
        },
        validator_set,
        public_checkpoint_proof,
    ))
}

fn validate_checkpoint_attachment(
    state: &SecondState,
    validator_set: &ValidatorSet,
    public_checkpoint_proof: Option<&PublicCurrencyCheckpointProof>,
) -> Result<(), PersistenceError> {
    let Some(proof) = public_checkpoint_proof else {
        return Ok(());
    };

    if proof.checkpoint().summary() != &state.public_currency_summary() {
        return Err(PersistenceError::CheckpointDoesNotMatchState);
    }

    if proof.validator_set_version() != validator_set.version() {
        return Err(PersistenceError::CheckpointValidatorSetMismatch {
            expected: validator_set.version(),
            actual: proof.validator_set_version(),
        });
    }

    Ok(())
}

fn push_len(out: &mut Vec<u8>, len: usize) -> Result<(), PersistenceError> {
    let len = u64::try_from(len).map_err(|_| PersistenceError::SnapshotTooLarge)?;
    out.extend_from_slice(&len.to_be_bytes());
    Ok(())
}

struct Decoder<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Decoder<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn read_u8(&mut self) -> Result<u8, PersistenceError> {
        Ok(self.read_exact(1)?[0])
    }

    fn read_u64(&mut self) -> Result<u64, PersistenceError> {
        Ok(u64::from_be_bytes(
            self.read_exact(8)?
                .try_into()
                .map_err(|_| PersistenceError::InvalidSnapshot)?,
        ))
    }

    fn read_u128(&mut self) -> Result<u128, PersistenceError> {
        Ok(u128::from_be_bytes(
            self.read_exact(16)?
                .try_into()
                .map_err(|_| PersistenceError::InvalidSnapshot)?,
        ))
    }

    fn read_array_32(&mut self) -> Result<[u8; 32], PersistenceError> {
        self.read_exact(32)?
            .try_into()
            .map_err(|_| PersistenceError::InvalidSnapshot)
    }

    fn read_len(&mut self) -> Result<usize, PersistenceError> {
        usize::try_from(self.read_u64()?).map_err(|_| PersistenceError::InvalidSnapshot)
    }

    fn read_exact(&mut self, len: usize) -> Result<&'a [u8], PersistenceError> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or(PersistenceError::InvalidSnapshot)?;

        if end > self.bytes.len() {
            return Err(PersistenceError::InvalidSnapshot);
        }

        let slice = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(slice)
    }

    fn finish(self) -> Result<(), PersistenceError> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(PersistenceError::InvalidSnapshot)
        }
    }
}
