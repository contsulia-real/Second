mod codec;
#[cfg(test)]
mod tests;

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::{MAX_LOCAL_PEER_CANDIDATES, NetworkError, NodeId, PeerRecord};
use crate::ValidatorId;

#[derive(Clone, Debug, Eq, PartialEq)]
struct CachedPeer {
    record: PeerRecord,
    validator: Option<ValidatorId>,
}

#[derive(Clone)]
pub(crate) struct PeerStore {
    path: Arc<PathBuf>,
    records: Arc<Mutex<Vec<CachedPeer>>>,
}

impl PeerStore {
    pub(crate) async fn recent_async(
        &self,
        limit: u16,
        excluded: Vec<NodeId>,
    ) -> Result<Vec<PeerRecord>, NetworkError> {
        let cache = self.clone();
        tokio::task::spawn_blocking(move || cache.recent(limit, &excluded))
            .await
            .map_err(|error| NetworkError::Transport(format!("peer cache worker failed: {error}")))
    }

    pub(crate) async fn validator_candidates_async(
        &self,
        allowed: Vec<ValidatorId>,
        excluded: NodeId,
    ) -> Result<Vec<PeerRecord>, NetworkError> {
        let cache = self.clone();
        tokio::task::spawn_blocking(move || cache.validator_candidates(&allowed, excluded))
            .await
            .map_err(|error| {
                NetworkError::Transport(format!("peer cache worker failed: {error}"))
            })?
    }

    pub(crate) fn load(path: PathBuf) -> Result<Self, NetworkError> {
        let records = match fs::File::open(&path) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take((codec::MAX_STORE_SIZE + 1) as u64)
                    .read_to_end(&mut bytes)
                    .map_err(peer_store_error)?;
                codec::decode(&bytes).unwrap_or_default()
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(peer_store_error(error)),
        };

        Ok(Self {
            path: Arc::new(path),
            records: Arc::new(Mutex::new(records)),
        })
    }

    pub(crate) fn recent(&self, limit: u16, excluded: &[NodeId]) -> Vec<PeerRecord> {
        let limit = usize::from(limit.min(MAX_LOCAL_PEER_CANDIDATES));
        self.records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .rev()
            .filter(|entry| !excluded.contains(&entry.record.node_id()))
            .take(limit)
            .map(|entry| entry.record.clone())
            .collect()
    }

    pub(crate) fn record_authenticated(&self, record: &PeerRecord) -> Result<(), NetworkError> {
        self.record(record, None)
    }

    pub(crate) fn record_validator_authenticated(
        &self,
        record: &PeerRecord,
        validator: ValidatorId,
    ) -> Result<(), NetworkError> {
        self.record(record, Some(validator))
    }

    pub(crate) fn validator_candidates(
        &self,
        allowed: &[ValidatorId],
        excluded: NodeId,
    ) -> Result<Vec<PeerRecord>, NetworkError> {
        let mut records = self
            .records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        if records
            .iter()
            .any(|entry| entry.validator.is_some_and(|id| !allowed.contains(&id)))
        {
            let mut updated = records.clone();
            for entry in &mut updated {
                if entry.validator.is_some_and(|id| !allowed.contains(&id)) {
                    entry.validator = None;
                }
            }
            persist_records(&self.path, &updated)?;
            *records = updated;
        }
        let mut candidates = records
            .iter()
            .rev()
            .filter(|entry| entry.record.node_id() != excluded)
            .collect::<Vec<_>>();
        candidates.sort_by_key(|entry| entry.validator.is_none());
        Ok(candidates
            .into_iter()
            .map(|entry| entry.record.clone())
            .collect())
    }

    fn record(
        &self,
        record: &PeerRecord,
        validator: Option<ValidatorId>,
    ) -> Result<(), NetworkError> {
        let mut records = self
            .records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let validator = validator.or_else(|| {
            records
                .iter()
                .find(|entry| entry.record.node_id() == record.node_id())
                .and_then(|entry| entry.validator)
        });
        let entry = CachedPeer {
            record: record.clone(),
            validator,
        };
        if records.last() == Some(&entry) {
            return Ok(());
        }

        let mut updated = records.clone();
        updated.retain(|existing| {
            existing.record.node_id() != record.node_id()
                && (validator.is_none() || existing.validator != validator)
        });
        if updated.len() == usize::from(MAX_LOCAL_PEER_CANDIDATES) {
            match updated.iter().position(|entry| entry.validator.is_none()) {
                Some(position) => {
                    updated.remove(position);
                }
                None if validator.is_none() => return Ok(()),
                None => {
                    updated.remove(0);
                }
            }
        }
        updated.push(entry);

        persist_records(&self.path, &updated)?;
        *records = updated;
        Ok(())
    }

    pub(crate) fn record_failure(&self, record: &PeerRecord) -> Result<(), NetworkError> {
        let mut records = self
            .records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let Some(position) = records
            .iter()
            .position(|existing| existing.record == *record)
        else {
            return Ok(());
        };
        if position == 0 {
            return Ok(());
        }

        let mut updated = records.clone();
        let failed = updated.remove(position);
        updated.insert(0, failed);
        persist_records(&self.path, &updated)?;
        *records = updated;
        Ok(())
    }
}

fn persist_records(path: &Path, records: &[CachedPeer]) -> Result<(), NetworkError> {
    let encoded = codec::encode(records)?;
    write_store(path, &encoded)
}

fn write_store(path: &Path, encoded: &[u8]) -> Result<(), NetworkError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(peer_store_error)?;
    }

    let staging = crate::persistence::slot::append_suffix(path, ".new");
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&staging)
        .map_err(peer_store_error)?;
    file.write_all(encoded).map_err(peer_store_error)?;
    file.sync_all().map_err(peer_store_error)?;
    drop(file);
    fs::rename(staging, path).map_err(peer_store_error)
}

fn peer_store_error(error: impl std::fmt::Display) -> NetworkError {
    NetworkError::PeerStore(error.to_string())
}
