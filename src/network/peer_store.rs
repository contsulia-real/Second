use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::{
    MAX_PEER_RECORDS, NetworkError, NetworkMessage, NodeId, PeerRecord, decode_network_message,
    encode_network_message,
};

#[derive(Clone)]
pub(crate) struct PeerStore {
    path: Arc<PathBuf>,
    records: Arc<Mutex<Vec<PeerRecord>>>,
}

impl PeerStore {
    pub(crate) fn load(path: PathBuf) -> Result<Self, NetworkError> {
        let records = match fs::read(&path) {
            Ok(bytes) => match decode_network_message(&bytes)? {
                NetworkMessage::Peers { records } => records,
                _ => {
                    return Err(NetworkError::PeerStore(
                        "invalid peer store payload".to_owned(),
                    ));
                }
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(peer_store_error(error)),
        };

        Ok(Self {
            path: Arc::new(path),
            records: Arc::new(Mutex::new(records)),
        })
    }

    pub(crate) fn recent(&self, limit: u16, excluded: &[NodeId]) -> Vec<PeerRecord> {
        let limit = usize::from(limit.min(MAX_PEER_RECORDS));
        self.records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .rev()
            .filter(|record| !excluded.contains(&record.node_id()))
            .take(limit)
            .cloned()
            .collect()
    }

    pub(crate) fn record_success(&self, record: &PeerRecord) -> Result<(), NetworkError> {
        let mut records = self
            .records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        records.retain(|existing| existing.node_id() != record.node_id());
        records.push(record.clone());
        if records.len() > usize::from(MAX_PEER_RECORDS) {
            records.remove(0);
        }

        let encoded = encode_network_message(&NetworkMessage::Peers {
            records: records.clone(),
        })?;
        write_store(&self.path, &encoded)
    }
}

fn write_store(path: &Path, encoded: &[u8]) -> Result<(), NetworkError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(peer_store_error)?;
    }

    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(peer_store_error)?;
    file.write_all(encoded).map_err(peer_store_error)?;
    file.sync_all().map_err(peer_store_error)
}

fn peer_store_error(error: impl std::fmt::Display) -> NetworkError {
    NetworkError::PeerStore(error.to_string())
}
