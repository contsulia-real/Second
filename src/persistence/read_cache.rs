//! Cache only immutable, validated results; every hit rechecks the durable commit and slot metadata.
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use super::{
    commit::{self, CommitReference},
    slot::slot_path,
};
use crate::PersistenceError;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ReadToken {
    reference: Option<CommitReference>,
    slots: [Option<(u64, SystemTime)>; 2],
}

impl ReadToken {
    pub fn read(base: &Path) -> Result<Self, PersistenceError> {
        let mut slots = [None, None];
        for (index, slot) in slots.iter_mut().enumerate() {
            match fs::metadata(slot_path(base, index as u64 + 1)) {
                Ok(metadata) => {
                    *slot = Some((
                        metadata.len(),
                        metadata.modified().map_err(PersistenceError::from_io)?,
                    ))
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(PersistenceError::from_io(error)),
            }
        }
        Ok(Self {
            reference: commit::read(base)?,
            slots,
        })
    }
}

#[derive(Debug)]
pub(super) struct ReadCache<T> {
    pub token: ReadToken,
    pub snapshot: Arc<T>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SecondState, StateStore, ValidatorCredential, ValidatorId, ValidatorSet};

    #[test]
    fn immutable_cache_observes_external_commits_and_does_not_mask_corruption() {
        let base = std::env::temp_dir().join(format!(
            "second-cache-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let public_key = |seed| {
            ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
                .verifying_key()
                .to_bytes()
        };
        let validators = ValidatorSet::new(
            1,
            [ValidatorCredential::new(
                ValidatorId::new(1),
                public_key(1),
                public_key(2),
                public_key(3),
            )
            .unwrap()],
        )
        .unwrap();
        let reader = StateStore::new(&base);
        reader
            .initialize(&SecondState::genesis([], 1), &validators)
            .unwrap();
        let first = reader.load_shared().unwrap().unwrap();
        assert!(Arc::ptr_eq(&first, &reader.load_shared().unwrap().unwrap()));
        StateStore::new(&base)
            .attach_checkpoint_proof(None)
            .unwrap();
        let second = reader.load_shared().unwrap().unwrap();
        assert_eq!(second.generation, first.generation + 1);
        assert!(!Arc::ptr_eq(&first, &second));
        let commit_path = super::super::slot::append_suffix(&base, ".commit");
        let committed = fs::read(&commit_path).unwrap();
        fs::write(&commit_path, b"partial").unwrap();
        assert!(reader.load_shared().is_err());
        assert!(StateStore::new(&base).load().is_err());
        fs::write(commit_path, committed).unwrap();
        for generation in [1, 2] {
            fs::write(reader.slot_path_for_generation(generation), b"corrupted").unwrap();
        }
        assert!(reader.load_shared().is_err());
        reader.remove_files().unwrap();
        let _ = fs::remove_file(base);
    }
}
