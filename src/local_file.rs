use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

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

pub(crate) fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = OsString::from(path.as_os_str());
    value.push(suffix);
    PathBuf::from(value)
}

pub(crate) fn write_new(path: &Path, bytes: &[u8], label: &str) -> Result<(), String> {
    write_new_with_mode(path, bytes, label, false)
}

pub(crate) fn write_new_private(path: &Path, bytes: &[u8], label: &str) -> Result<(), String> {
    write_new_with_mode(path, bytes, label, true)
}

fn write_new_with_mode(
    path: &Path,
    bytes: &[u8],
    label: &str,
    private: bool,
) -> Result<(), String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create {label} directory {}: {error}",
                parent.display()
            )
        })?;
    }

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = private;

    let mut file = options
        .open(path)
        .map_err(|error| format!("failed to create {label} {}: {error}", path.display()))?;
    file.write_all(bytes)
        .map_err(|error| format!("failed to write {label} {}: {error}", path.display()))?;
    file.sync_all()
        .map_err(|error| format!("failed to sync {label} {}: {error}", path.display()))
}
