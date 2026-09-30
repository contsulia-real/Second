use std::ffi::OsString;
use std::fs;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use second::{MAX_PEER_RECORDS, NodeId, PeerRecord};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BootstrapRecordFile {
    node_id: String,
    address: String,
    certificate_base64: String,
}

pub(crate) fn load(snapshot_base: &str) -> Result<Vec<PeerRecord>, String> {
    let path = bootstrap_path(Path::new(snapshot_base));
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(format!(
                "failed to read bootstrap file {}: {error}",
                path.display()
            ));
        }
    };

    let records = serde_json::from_str::<Vec<BootstrapRecordFile>>(&contents)
        .map_err(|error| format!("invalid bootstrap file {}: {error}", path.display()))?;
    if records.len() > usize::from(MAX_PEER_RECORDS) {
        return Err(format!(
            "bootstrap file {} contains {} records; maximum is {}",
            path.display(),
            records.len(),
            MAX_PEER_RECORDS
        ));
    }

    records
        .into_iter()
        .enumerate()
        .map(|(index, record)| parse_record(&path, index, record))
        .collect()
}

pub(crate) fn bootstrap_path(snapshot_base: &Path) -> PathBuf {
    let mut path = OsString::from(snapshot_base.as_os_str());
    path.push(".bootstrap.json");
    PathBuf::from(path)
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
