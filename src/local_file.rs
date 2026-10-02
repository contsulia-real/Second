use std::fs::File;
use std::io::Read as _;
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

pub(crate) fn read_bounded(path: &Path, maximum: usize, label: &str) -> Result<Vec<u8>, String> {
    let file = File::open(path)
        .map_err(|error| format!("failed to read {label} {}: {error}", path.display()))?;
    let limit = u64::try_from(maximum)
        .expect("local file size limit must fit u64")
        .saturating_add(1);
    let mut bytes = Vec::new();
    file.take(limit)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("failed to read {label} {}: {error}", path.display()))?;
    if bytes.len() > maximum {
        return Err(format!(
            "{label} {} is too large; maximum is {maximum} bytes",
            path.display()
        ));
    }
    Ok(bytes)
}

pub(crate) fn decode_standard_base64_32(value: &str) -> Result<[u8; 32], &'static str> {
    let decoded = STANDARD
        .decode(value)
        .map_err(|_| "must be valid standard base64")?;
    decoded
        .try_into()
        .map_err(|_| "must decode to exactly 32 bytes")
}
