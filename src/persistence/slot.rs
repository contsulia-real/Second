use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use crate::PersistenceError;

use super::PersistedNodeState;
use super::codec::{MAX_SNAPSHOT_FILE_SIZE, decode_snapshot};

type PathLockMap = BTreeMap<PathBuf, Weak<Mutex<()>>>;

pub(super) fn shared_path_lock(path: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<PathLockMap>> = OnceLock::new();

    let mut locks = LOCKS
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    if let Some(existing) = locks.get(path).and_then(Weak::upgrade) {
        return existing;
    }

    locks.retain(|_, lock| lock.strong_count() > 0);
    let lock = Arc::new(Mutex::new(()));
    locks.insert(path.to_path_buf(), Arc::downgrade(&lock));
    lock
}

pub(super) fn slot_path(base_path: &Path, generation: u64) -> PathBuf {
    if generation % 2 == 1 {
        append_suffix(base_path, ".a")
    } else {
        append_suffix(base_path, ".b")
    }
}

pub(super) fn remove_slots(base_path: &Path) -> Result<(), PersistenceError> {
    for path in [
        append_suffix(base_path, ".a"),
        append_suffix(base_path, ".b"),
    ] {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(PersistenceError::from_io(error)),
        }
    }
    Ok(())
}

pub(super) fn load_latest(
    base_path: &Path,
) -> Result<Option<PersistedNodeState>, PersistenceError> {
    let mut any_file_exists = false;
    let mut valid = Vec::new();

    for path in [
        append_suffix(base_path, ".a"),
        append_suffix(base_path, ".b"),
    ] {
        match read_slot(&path) {
            Ok(Some(snapshot)) => {
                any_file_exists = true;
                valid.push(snapshot);
            }
            Ok(None) => {}
            Err(PersistenceError::Io(io::ErrorKind::NotFound)) => {}
            Err(PersistenceError::Io(kind)) => return Err(PersistenceError::Io(kind)),
            Err(_) => {
                any_file_exists = true;
            }
        }
    }

    if let Some(latest) = valid.into_iter().max_by_key(|snapshot| snapshot.generation) {
        return Ok(Some(latest));
    }

    if any_file_exists {
        Err(PersistenceError::NoValidSnapshot)
    } else {
        Ok(None)
    }
}

pub(super) fn write_slot(
    base_path: &Path,
    generation: u64,
    bytes: &[u8],
) -> Result<(), PersistenceError> {
    if let Some(parent) = base_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(PersistenceError::from_io)?;
    }

    let path = slot_path(base_path, generation);
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .map_err(PersistenceError::from_io)?;
    file.write_all(bytes).map_err(PersistenceError::from_io)?;
    file.sync_all().map_err(PersistenceError::from_io)?;
    Ok(())
}

fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value: OsString = path.as_os_str().to_owned();
    value.push(suffix);
    PathBuf::from(value)
}

fn read_slot(path: &Path) -> Result<Option<PersistedNodeState>, PersistenceError> {
    let mut file = File::open(path).map_err(PersistenceError::from_io)?;
    let file_len = file.metadata().map_err(PersistenceError::from_io)?.len();
    if file_len > MAX_SNAPSHOT_FILE_SIZE {
        return Err(PersistenceError::SnapshotTooLarge);
    }

    let capacity = usize::try_from(file_len).map_err(|_| PersistenceError::SnapshotTooLarge)?;
    let mut bytes = Vec::with_capacity(capacity);
    file.read_to_end(&mut bytes)
        .map_err(PersistenceError::from_io)?;

    decode_snapshot(&bytes).map(Some)
}
