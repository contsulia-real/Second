//! Encrypted wallet bytes reuse the node's locks, slots and durable commit reference.
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::{commit, slot};
use crate::PersistenceError;

const MAX_BLOB_BYTES: usize = 64 * 1024 * 1024;

pub struct DurableBlobStore {
    base: PathBuf,
}

impl DurableBlobStore {
    pub fn new(base: impl Into<PathBuf>) -> Self {
        Self { base: base.into() }
    }

    pub fn load(&self) -> Result<Option<(u64, Vec<u8>)>, PersistenceError> {
        let mutex = slot::shared_path_lock(&self.base);
        let _guard = mutex
            .lock()
            .map_err(|_| PersistenceError::StoreLockPoisoned)?;
        let _file_lock = slot::lock_store_file(&self.base)?;
        self.load_locked()
    }

    pub fn save(&self, expected: Option<u64>, bytes: &[u8]) -> Result<u64, PersistenceError> {
        if bytes.len() > MAX_BLOB_BYTES {
            return Err(PersistenceError::SnapshotTooLarge);
        }
        let mutex = slot::shared_path_lock(&self.base);
        let _guard = mutex
            .lock()
            .map_err(|_| PersistenceError::StoreLockPoisoned)?;
        let _file_lock = slot::lock_store_file(&self.base)?;
        let current = self.load_locked()?.map(|(generation, _)| generation);
        if current != expected {
            return Err(PersistenceError::StaleState);
        }
        let generation = current
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(PersistenceError::GenerationOverflow)?;
        let mut encoded = bytes.to_vec();
        encoded.extend_from_slice(&Sha256::digest(bytes));
        slot::write_slots(&self.base, generation, &encoded)?;
        Ok(generation)
    }

    fn load_locked(&self) -> Result<Option<(u64, Vec<u8>)>, PersistenceError> {
        let Some(reference) = commit::read(&self.base)? else {
            for suffix in [".a", ".b"] {
                if slot::append_suffix(&self.base, suffix)
                    .try_exists()
                    .map_err(PersistenceError::from_io)?
                {
                    return Err(PersistenceError::NoValidSnapshot);
                }
            }
            return Ok(None);
        };
        for suffix in [".a", ".b"] {
            let path = slot::append_suffix(&self.base, suffix);
            if let Some(bytes) = read_committed(&path, &reference.checksum)? {
                return Ok(Some((reference.generation, bytes)));
            }
        }
        Err(PersistenceError::NoValidSnapshot)
    }
}

fn read_committed(path: &Path, checksum: &[u8; 32]) -> Result<Option<Vec<u8>>, PersistenceError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(PersistenceError::from_io(error)),
    };
    let mut bytes = Vec::new();
    file.take((MAX_BLOB_BYTES + 33) as u64)
        .read_to_end(&mut bytes)
        .map_err(PersistenceError::from_io)?;
    if bytes.len() < 32 || bytes.len() > MAX_BLOB_BYTES + 32 {
        return Ok(None);
    }
    let split = bytes.len() - 32;
    if &bytes[split..] != checksum || Sha256::digest(&bytes[..split]).as_slice() != checksum {
        return Ok(None);
    }
    bytes.truncate(split);
    Ok(Some(bytes))
}
