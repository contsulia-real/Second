use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use second::{MAX_LOCAL_PEER_CANDIDATES, NodeId, PeerRecord};
use serde::{Deserialize, Serialize};

use crate::local_file::{append_suffix, read_bounded, write_new};

const MAX_BOOTSTRAP_FILE_SIZE: usize = 256 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BootstrapRecordFile {
    node_id: String,
    address: String,
    certificate_base64: String,
}

pub(crate) fn load(snapshot_base: &str) -> Result<Vec<PeerRecord>, String> {
    let path = bootstrap_path(Path::new(snapshot_base));
    if !path.try_exists().map_err(|error| {
        format!(
            "failed to inspect bootstrap file {}: {error}",
            path.display()
        )
    })? {
        return Ok(Vec::new());
    }
    let contents = read_bounded(&path, MAX_BOOTSTRAP_FILE_SIZE, "bootstrap file")?;

    let records = serde_json::from_slice::<Vec<BootstrapRecordFile>>(&contents)
        .map_err(|error| format!("invalid bootstrap file {}: {error}", path.display()))?;
    if records.len() > usize::from(MAX_LOCAL_PEER_CANDIDATES) {
        return Err(format!(
            "bootstrap file {} contains {} records; maximum is {}",
            path.display(),
            records.len(),
            MAX_LOCAL_PEER_CANDIDATES
        ));
    }

    records
        .into_iter()
        .enumerate()
        .map(|(index, record)| parse_record(&path, index, record))
        .collect()
}

pub(crate) fn write(snapshot_base: &Path, records: &[PeerRecord]) -> Result<(), String> {
    if records.len() > usize::from(MAX_LOCAL_PEER_CANDIDATES) {
        return Err(format!(
            "bootstrap record count {} exceeds maximum {}",
            records.len(),
            MAX_LOCAL_PEER_CANDIDATES
        ));
    }
    let file = records
        .iter()
        .map(|record| BootstrapRecordFile {
            node_id: record.node_id().to_string(),
            address: record.address().to_string(),
            certificate_base64: STANDARD.encode(record.certificate_der()),
        })
        .collect::<Vec<_>>();
    let bytes = serde_json::to_vec_pretty(&file)
        .map_err(|error| format!("failed to encode bootstrap records: {error}"))?;
    write_new(&bootstrap_path(snapshot_base), &bytes, "bootstrap file")
}

pub(crate) fn bootstrap_path(snapshot_base: &Path) -> PathBuf {
    append_suffix(snapshot_base, ".bootstrap.json")
}

fn parse_record(
    path: &Path,
    index: usize,
    record: BootstrapRecordFile,
) -> Result<PeerRecord, String> {
    let node_id = parse_node_id(&record.node_id).map_err(|error| {
        format!(
            "invalid bootstrap file {} record {index} node_id: {error}",
            path.display()
        )
    })?;
    let address = record.address.parse::<SocketAddr>().map_err(|error| {
        format!(
            "invalid bootstrap file {} record {index} address {:?}: {error}",
            path.display(),
            record.address
        )
    })?;
    let certificate = STANDARD
        .decode(&record.certificate_base64)
        .map_err(|error| {
            format!(
                "invalid bootstrap file {} record {index} certificate_base64: {error}",
                path.display()
            )
        })?;

    PeerRecord::new(node_id, address, certificate).map_err(|error| {
        format!(
            "invalid bootstrap file {} record {index}: {error:?}",
            path.display()
        )
    })
}

fn parse_node_id(value: &str) -> Result<NodeId, &'static str> {
    if value.len() != 64 {
        return Err("must be exactly 64 lowercase hexadecimal characters");
    }

    let bytes = value.as_bytes();
    let mut decoded = [0_u8; 32];
    for (index, output) in decoded.iter_mut().enumerate() {
        let high = hex_nibble(bytes[index * 2])
            .ok_or("must be exactly 64 lowercase hexadecimal characters")?;
        let low = hex_nibble(bytes[index * 2 + 1])
            .ok_or("must be exactly 64 lowercase hexadecimal characters")?;
        *output = (high << 4) | low;
    }

    Ok(NodeId::from_bytes(decoded))
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}
