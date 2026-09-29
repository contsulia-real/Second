use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use crate::{PersistenceError, SecondState, ValidatorSet};

use super::PersistedNodeState;
use super::codec::{MAX_SNAPSHOT_FILE_SIZE, decode_snapshot, encode_snapshot};

#[derive(Clone, Debug)]
pub struct StateStore {
    base_path: PathBuf,
}

impl StateStore {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            base_path: path.as_ref().to_path_buf(),
        }
    }

    pub fn save(
        &self,
        state: &SecondState,
        validator_set: &ValidatorSet,
    ) -> Result<u64, PersistenceError> {
        let latest_generation = match self.load()? {
            Some(snapshot) => snapshot.generation,
            None => 0,
        };

        let generation = latest_generation
            .checked_add(1)
            .ok_or(PersistenceError::GenerationOverflow)?;
        let bytes = encode_snapshot(generation, state, validator_set)?;

        if let Some(parent) = self
            .base_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(PersistenceError::from_io)?;
        }

        let path = self.slot_path_for_generation(generation);
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path)
            .map_err(PersistenceError::from_io)?;
        file.write_all(&bytes).map_err(PersistenceError::from_io)?;
        file.sync_all().map_err(PersistenceError::from_io)?;

        Ok(generation)
    }

    pub fn load(&self) -> Result<Option<PersistedNodeState>, PersistenceError> {
        let mut any_file_exists = false;
        let mut valid = Vec::new();

        for path in [self.slot_a_path(), self.slot_b_path()] {
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

    pub fn slot_path_for_generation(&self, generation: u64) -> PathBuf {
        if generation % 2 == 1 {
            self.slot_a_path()
        } else {
            self.slot_b_path()
        }
    }

    pub fn remove_files(&self) -> Result<(), PersistenceError> {
        for path in [self.slot_a_path(), self.slot_b_path()] {
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(PersistenceError::from_io(error)),
            }
        }
        Ok(())
    }

    fn slot_a_path(&self) -> PathBuf {
        append_suffix(&self.base_path, ".a")
    }

    fn slot_b_path(&self) -> PathBuf {
        append_suffix(&self.base_path, ".b")
    }
}

fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value: OsString = path.as_os_str().to_owned();
    value.push(suffix);
    PathBuf::from(value)
}

fn read_slot(path: &Path) -> Result<Option<PersistedNodeState>, PersistenceError> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) => return Err(PersistenceError::from_io(error)),
    };

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
