use std::ffi::OsString;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};

use crate::public_state_codec::{
    PUBLIC_CURRENCY_STATE_ENCODED_SIZE, PUBLIC_CURRENCY_SUMMARY_ENCODED_SIZE,
    decode_public_currency_state, decode_public_currency_summary, encode_public_currency_state,
    encode_public_currency_summary,
};
use crate::{
    CertifiedPublicCurrencyCheckpoint, PersistenceError, PublicCurrencyCheckpointProof,
    PublicCurrencyView, ValidatorRegistry, ValidatorSet, ValidatorSetTransitionProof,
};

use super::codec::{Decoder, push_len};
use super::slot::{lock_store_file, shared_path_lock, write_snapshot_file};
use super::validator_codec::{
    decode_validator_registry, decode_validator_set, encode_validator_registry,
    encode_validator_set,
};
use super::{
    commit::{self, CommitReference},
    read_cache::{ReadCache, ReadToken},
};

const PUBLIC_SNAPSHOT_MAGIC: [u8; 4] = *b"SPUB";
const PUBLIC_SNAPSHOT_VERSION: u32 = 1;
const PUBLIC_SNAPSHOT_CHECKSUM_SIZE: usize = 32;
const PUBLIC_SNAPSHOT_HEADER_SIZE: usize = 4 + 4 + 8 + 8;
const MAX_PUBLIC_SNAPSHOT_PAYLOAD_SIZE: u64 = 128 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PersistedPublicNodeState {
    pub validator_set: ValidatorSet,
    pub validator_registry: ValidatorRegistry,
    pub view: Option<PublicCurrencyView>,
    pub checkpoint_proof: Option<PublicCurrencyCheckpointProof>,
    pub generation: u64,
}

#[derive(Clone)]
pub struct PublicStateStore {
    base_path: PathBuf,
    data_path: PathBuf,
    lock_path: PathBuf,
    process_lock: Arc<Mutex<()>>,
    read_cache: Arc<Mutex<Option<ReadCache<PersistedPublicNodeState>>>>,
}

impl PublicStateStore {
    pub fn new(base_path: impl AsRef<Path>) -> Self {
        let base_path = base_path.as_ref().to_path_buf();
        let data_path = append_suffix(&base_path, ".public");
        let lock_path = append_suffix(&data_path, ".lock");
        let process_lock = shared_path_lock(&lock_path);
        Self {
            base_path,
            data_path,
            lock_path,
            process_lock,
            read_cache: Arc::new(Mutex::new(None)),
        }
    }

    pub fn base_path(&self) -> &Path {
        &self.base_path
    }

    pub fn load(&self) -> Result<Option<PersistedPublicNodeState>, PersistenceError> {
        Ok(self.load_shared()?.map(|snapshot| (*snapshot).clone()))
    }

    pub(crate) fn load_shared(
        &self,
    ) -> Result<Option<Arc<PersistedPublicNodeState>>, PersistenceError> {
        let _guard = self
            .process_lock
            .lock()
            .map_err(|_| PersistenceError::StoreLockPoisoned)?;
        let lock_file = lock_store_file(&self.lock_path)?;
        let result = self.load_shared_unlocked();
        let _ = File::unlock(&lock_file);
        result
    }

    pub fn initialize(
        &self,
        validator_set: ValidatorSet,
        validator_registry: ValidatorRegistry,
    ) -> Result<u64, PersistenceError> {
        let _guard = self
            .process_lock
            .lock()
            .map_err(|_| PersistenceError::StoreLockPoisoned)?;
        let lock_file = lock_store_file(&self.lock_path)?;
        let result = (|| {
            if self.load_unlocked()?.is_some() {
                return Err(PersistenceError::AlreadyInitialized);
            }
            validator_registry
                .validate_current_set(&validator_set)
                .map_err(|_| PersistenceError::ValidatorRegistryMismatch)?;
            let state = PersistedPublicNodeState {
                validator_set,
                validator_registry,
                view: None,
                checkpoint_proof: None,
                generation: 1,
            };
            self.write_unlocked(&state)?;
            Ok(1)
        })();
        let _ = File::unlock(&lock_file);
        result
    }

    pub fn install_certified_view(
        &self,
        view: PublicCurrencyView,
        checkpoint: &CertifiedPublicCurrencyCheckpoint,
    ) -> Result<u64, PersistenceError> {
        let _guard = self
            .process_lock
            .lock()
            .map_err(|_| PersistenceError::StoreLockPoisoned)?;
        let lock_file = lock_store_file(&self.lock_path)?;
        let result = (|| {
            let mut current = self
                .load_unlocked()?
                .ok_or(PersistenceError::MissingSnapshot)?;
            checkpoint
                .verify_view(&view, &current.validator_set)
                .map_err(PersistenceError::PublicCheckpoint)?;
            if let Some(existing) = current.checkpoint_proof.as_ref() {
                let minimum = existing.checkpoint().epoch();
                let actual = checkpoint.checkpoint().epoch();
                if actual < minimum {
                    return Err(PersistenceError::StaleCheckpointEpoch { minimum, actual });
                }
            }
            current.generation = current
                .generation
                .checked_add(1)
                .ok_or(PersistenceError::GenerationOverflow)?;
            current.view = Some(view);
            current.checkpoint_proof = Some(checkpoint.to_unverified_proof());
            let generation = current.generation;
            self.write_unlocked(&current)?;
            Ok(generation)
        })();
        let _ = File::unlock(&lock_file);
        result
    }

    pub fn remove_files(&self) -> Result<(), PersistenceError> {
        let _guard = self
            .process_lock
            .lock()
            .map_err(|_| PersistenceError::StoreLockPoisoned)?;
        let lock_file = lock_store_file(&self.lock_path)?;
        for path in [
            slot_path(&self.data_path, 1),
            slot_path(&self.data_path, 2),
            append_suffix(&self.data_path, ".commit"),
        ] {
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    let _ = File::unlock(&lock_file);
                    return Err(PersistenceError::from_io(error));
                }
            }
        }
        let _ = File::unlock(&lock_file);
        drop(lock_file);
        match fs::remove_file(&self.lock_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(PersistenceError::from_io(error)),
        }
    }

    pub fn activate_validator_set_transition(
        &self,
        proof: &ValidatorSetTransitionProof,
    ) -> Result<u64, PersistenceError> {
        let _guard = self
            .process_lock
            .lock()
            .map_err(|_| PersistenceError::StoreLockPoisoned)?;
        let lock_file = lock_store_file(&self.lock_path)?;
        let result = (|| {
            let mut current = self
                .load_unlocked()?
                .ok_or(PersistenceError::MissingSnapshot)?;
            let certified = proof
                .verify(&current.validator_set, &current.validator_registry)
                .map_err(|error| match error {
                    crate::ValidatorTransitionSourceError::Transition(error) => {
                        PersistenceError::ValidatorTransition(error)
                    }
                    crate::ValidatorTransitionSourceError::Admission(_) => {
                        PersistenceError::InvalidSnapshot
                    }
                })?;
            let next_set = certified
                .activate(&mut current.validator_registry)
                .map_err(PersistenceError::ValidatorTransition)?;
            current.validator_set = next_set;
            current.view = None;
            current.checkpoint_proof = None;
            current.generation = current
                .generation
                .checked_add(1)
                .ok_or(PersistenceError::GenerationOverflow)?;
            let generation = current.generation;
            self.write_unlocked(&current)?;
            Ok(generation)
        })();
        let _ = File::unlock(&lock_file);
        result
    }

    fn load_unlocked(&self) -> Result<Option<PersistedPublicNodeState>, PersistenceError> {
        Ok(self
            .load_shared_unlocked()?
            .map(|snapshot| (*snapshot).clone()))
    }

    fn load_shared_unlocked(
        &self,
    ) -> Result<Option<Arc<PersistedPublicNodeState>>, PersistenceError> {
        let token = ReadToken::read(&self.data_path)?;
        let mut cache = self
            .read_cache
            .lock()
            .map_err(|_| PersistenceError::StoreLockPoisoned)?;
        if let Some(cached) = cache.as_ref().filter(|cached| cached.token == token) {
            return Ok(Some(Arc::clone(&cached.snapshot)));
        }
        *cache = None;
        let snapshot = self.read_slots_unlocked()?.map(Arc::new);
        if let Some(snapshot) = &snapshot {
            *cache = Some(ReadCache {
                token,
                snapshot: Arc::clone(snapshot),
            });
        }
        Ok(snapshot)
    }

    fn read_slots_unlocked(&self) -> Result<Option<PersistedPublicNodeState>, PersistenceError> {
        let committed = commit::read(&self.data_path)?;
        let mut found = Vec::new();
        let mut any_exists = false;
        for path in [slot_path(&self.data_path, 1), slot_path(&self.data_path, 2)] {
            match read_slot(&path) {
                Ok(Some((state, checksum))) => {
                    any_exists = true;
                    found.push((state, checksum));
                }
                Ok(None) => {}
                Err(PersistenceError::Io(std::io::ErrorKind::NotFound)) => {}
                Err(_) => any_exists = true,
            }
        }
        if found.len() == 2
            && found[0].0.generation == found[1].0.generation
            && found[0].1 != found[1].1
        {
            return Err(PersistenceError::ConflictingSnapshotGeneration(
                found[0].0.generation,
            ));
        }
        if let Some((state, _)) = found
            .into_iter()
            .filter(|(state, checksum)| {
                committed.is_some_and(|reference| {
                    reference.generation == state.generation && &reference.checksum == checksum
                })
            })
            .max_by_key(|item| item.0.generation)
        {
            return Ok(Some(state));
        }
        if any_exists || committed.is_some() {
            Err(PersistenceError::NoValidSnapshot)
        } else {
            Ok(None)
        }
    }

    fn write_unlocked(&self, state: &PersistedPublicNodeState) -> Result<(), PersistenceError> {
        let bytes = encode_snapshot(state)?;
        let primary = slot_path(&self.data_path, state.generation);
        let mirror = mirror_slot_path(&self.data_path, state.generation);
        write_snapshot_file(&primary, &bytes)?;
        commit::publish(
            &self.data_path,
            CommitReference {
                generation: state.generation,
                checksum: bytes[bytes.len() - 32..]
                    .try_into()
                    .map_err(|_| PersistenceError::InvalidSnapshot)?,
            },
        )?;
        let _ = write_snapshot_file(&mirror, &bytes);
        Ok(())
    }
}

fn encode_snapshot(state: &PersistedPublicNodeState) -> Result<Vec<u8>, PersistenceError> {
    validate_public_state(state)?;
    let mut payload = Vec::new();
    encode_validator_set(&mut payload, &state.validator_set)?;
    encode_validator_registry(&mut payload, &state.validator_registry)?;
    match (&state.view, &state.checkpoint_proof) {
        (None, None) => payload.push(0),
        (Some(view), Some(proof)) => {
            payload.push(1);
            encode_public_currency_summary(&mut payload, &view.summary);
            push_len(&mut payload, view.states.len())?;
            for public_state in &view.states {
                encode_public_currency_state(&mut payload, public_state);
            }
            let proof_bytes = proof
                .encode_bytes()
                .map_err(|_| PersistenceError::InvalidSnapshot)?;
            push_len(&mut payload, proof_bytes.len())?;
            payload.extend_from_slice(&proof_bytes);
        }
        _ => return Err(PersistenceError::InvalidSnapshot),
    }

    let payload_len =
        u64::try_from(payload.len()).map_err(|_| PersistenceError::SnapshotTooLarge)?;
    if payload_len > MAX_PUBLIC_SNAPSHOT_PAYLOAD_SIZE {
        return Err(PersistenceError::SnapshotTooLarge);
    }
    let mut out = Vec::with_capacity(
        PUBLIC_SNAPSHOT_HEADER_SIZE + payload.len() + PUBLIC_SNAPSHOT_CHECKSUM_SIZE,
    );
    out.extend_from_slice(&PUBLIC_SNAPSHOT_MAGIC);
    out.extend_from_slice(&PUBLIC_SNAPSHOT_VERSION.to_be_bytes());
    out.extend_from_slice(&state.generation.to_be_bytes());
    out.extend_from_slice(&payload_len.to_be_bytes());
    out.extend_from_slice(&payload);
    let checksum: [u8; 32] = Sha256::digest(&out).into();
    out.extend_from_slice(&checksum);
    Ok(out)
}

fn decode_snapshot(bytes: &[u8]) -> Result<(PersistedPublicNodeState, [u8; 32]), PersistenceError> {
    if bytes.len() < PUBLIC_SNAPSHOT_HEADER_SIZE + PUBLIC_SNAPSHOT_CHECKSUM_SIZE {
        return Err(PersistenceError::InvalidSnapshot);
    }
    if bytes[0..4] != PUBLIC_SNAPSHOT_MAGIC {
        return Err(PersistenceError::InvalidSnapshot);
    }
    let version = u32::from_be_bytes(
        bytes[4..8]
            .try_into()
            .map_err(|_| PersistenceError::InvalidSnapshot)?,
    );
    if version != PUBLIC_SNAPSHOT_VERSION {
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
    if payload_len > MAX_PUBLIC_SNAPSHOT_PAYLOAD_SIZE {
        return Err(PersistenceError::SnapshotTooLarge);
    }
    let payload_len =
        usize::try_from(payload_len).map_err(|_| PersistenceError::SnapshotTooLarge)?;
    let checksum_offset = PUBLIC_SNAPSHOT_HEADER_SIZE
        .checked_add(payload_len)
        .ok_or(PersistenceError::SnapshotTooLarge)?;
    let expected_len = checksum_offset
        .checked_add(PUBLIC_SNAPSHOT_CHECKSUM_SIZE)
        .ok_or(PersistenceError::SnapshotTooLarge)?;
    if bytes.len() != expected_len {
        return Err(PersistenceError::InvalidSnapshot);
    }
    let expected_checksum: [u8; 32] = Sha256::digest(&bytes[..checksum_offset]).into();
    let actual_checksum: [u8; 32] = bytes[checksum_offset..]
        .try_into()
        .map_err(|_| PersistenceError::InvalidSnapshot)?;
    if actual_checksum != expected_checksum {
        return Err(PersistenceError::ChecksumMismatch);
    }

    let mut decoder = Decoder::new(&bytes[PUBLIC_SNAPSHOT_HEADER_SIZE..checksum_offset]);
    let validator_set = decode_validator_set(&mut decoder)?;
    let validator_registry = decode_validator_registry(&mut decoder)?;
    let (view, checkpoint_proof) = match decoder.read_u8()? {
        0 => (None, None),
        1 => {
            let summary = decode_public_currency_summary(
                decoder.read_exact(PUBLIC_CURRENCY_SUMMARY_ENCODED_SIZE)?,
            )
            .map_err(|_| PersistenceError::InvalidSnapshot)?;
            let count = decoder.read_len()?;
            if count > decoder.remaining() / PUBLIC_CURRENCY_STATE_ENCODED_SIZE {
                return Err(PersistenceError::InvalidSnapshot);
            }
            let mut states = Vec::with_capacity(count);
            for _ in 0..count {
                states.push(
                    decode_public_currency_state(
                        decoder.read_exact(PUBLIC_CURRENCY_STATE_ENCODED_SIZE)?,
                    )
                    .map_err(|_| PersistenceError::InvalidSnapshot)?,
                );
            }
            let view = PublicCurrencyView::new(summary, states)
                .map_err(|_| PersistenceError::InvalidSnapshot)?;
            let proof_len = decoder.read_len()?;
            let proof = PublicCurrencyCheckpointProof::decode_bytes(decoder.read_exact(proof_len)?)
                .map_err(|_| PersistenceError::InvalidSnapshot)?;
            (Some(view), Some(proof))
        }
        _ => return Err(PersistenceError::InvalidSnapshot),
    };
    decoder.finish()?;
    let state = PersistedPublicNodeState {
        validator_set,
        validator_registry,
        view,
        checkpoint_proof,
        generation,
    };
    validate_public_state(&state)?;
    Ok((state, actual_checksum))
}

fn validate_public_state(state: &PersistedPublicNodeState) -> Result<(), PersistenceError> {
    state
        .validator_registry
        .validate_current_set(&state.validator_set)
        .map_err(|_| PersistenceError::ValidatorRegistryMismatch)?;
    match (&state.view, &state.checkpoint_proof) {
        (None, None) => Ok(()),
        (Some(view), Some(proof)) => {
            let certified = proof
                .clone()
                .verify_checkpoint(&state.validator_set)
                .map_err(PersistenceError::PublicCheckpoint)?;
            certified
                .verify_view(view, &state.validator_set)
                .map_err(PersistenceError::PublicCheckpoint)
        }
        _ => Err(PersistenceError::InvalidSnapshot),
    }
}

fn read_slot(
    path: &Path,
) -> Result<Option<(PersistedPublicNodeState, [u8; 32])>, PersistenceError> {
    let mut file = File::open(path).map_err(PersistenceError::from_io)?;
    let len = file.metadata().map_err(PersistenceError::from_io)?.len();
    let max_len = MAX_PUBLIC_SNAPSHOT_PAYLOAD_SIZE
        + (PUBLIC_SNAPSHOT_HEADER_SIZE + PUBLIC_SNAPSHOT_CHECKSUM_SIZE) as u64;
    if len > max_len {
        return Err(PersistenceError::SnapshotTooLarge);
    }
    let capacity = usize::try_from(len).map_err(|_| PersistenceError::SnapshotTooLarge)?;
    let mut bytes = Vec::with_capacity(capacity);
    file.read_to_end(&mut bytes)
        .map_err(PersistenceError::from_io)?;
    decode_snapshot(&bytes).map(Some)
}

fn slot_path(path: &Path, generation: u64) -> PathBuf {
    append_suffix(path, if generation % 2 == 1 { ".a" } else { ".b" })
}

fn mirror_slot_path(path: &Path, generation: u64) -> PathBuf {
    append_suffix(path, if generation % 2 == 1 { ".b" } else { ".a" })
}

fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value: OsString = path.as_os_str().to_owned();
    value.push(suffix);
    PathBuf::from(value)
}
