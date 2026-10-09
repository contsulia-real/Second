//! The durable commit reference prevents an older recovery slot from reviving signing state.
use std::fs;
use std::io::{Read, Write};
use std::path::Path;

use sha2::{Digest, Sha256};

use super::slot::append_suffix;
use crate::PersistenceError;

const MAGIC: &[u8; 8] = b"S2CMTV1\0";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct CommitReference {
    pub generation: u64,
    pub checksum: [u8; 32],
}

pub(super) fn read(base: &Path) -> Result<Option<CommitReference>, PersistenceError> {
    let file = match fs::File::open(append_suffix(base, ".commit")) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(PersistenceError::from_io(error)),
    };
    let mut bytes = Vec::new();
    file.take(81)
        .read_to_end(&mut bytes)
        .map_err(PersistenceError::from_io)?;
    if bytes.len() != 80
        || &bytes[..8] != MAGIC
        || Sha256::digest(&bytes[..48]).as_slice() != &bytes[48..]
    {
        return Err(PersistenceError::InvalidSnapshot);
    }
    Ok(Some(CommitReference {
        generation: u64::from_be_bytes(bytes[8..16].try_into().unwrap()),
        checksum: bytes[16..48].try_into().unwrap(),
    }))
}

pub(super) fn publish(base: &Path, reference: CommitReference) -> Result<(), PersistenceError> {
    let mut bytes = Vec::with_capacity(80);
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&reference.generation.to_be_bytes());
    bytes.extend_from_slice(&reference.checksum);
    let checksum = Sha256::digest(&bytes);
    bytes.extend_from_slice(&checksum);
    // Rewrite the fixed-size record under the store lock. sync_all flushes the
    // existing file, without relying on Windows rename metadata durability.
    // An interrupted write fails its checksum and closes signing.
    let mut file = super::slot::open_store_file(
        &append_suffix(base, ".commit"),
        fs::OpenOptions::new().truncate(false).write(true),
    )?;
    file.write_all(&bytes).map_err(PersistenceError::from_io)?;
    file.sync_all().map_err(PersistenceError::from_io)?;
    #[cfg(unix)]
    if let Some(parent) = base
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(PersistenceError::from_io)?;
    }
    Ok(())
}
