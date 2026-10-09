use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::SigningKey;

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

/// Keep the descriptor alive for the entire node or offline installation.
/// The empty lock file remains: deleting it could split ownership across inodes.
pub(crate) fn lock_node_directory(base: &Path) -> Result<File, String> {
    let parent = base
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create node directory: {error}"))?;
    let parent = fs::canonicalize(parent)
        .map_err(|error| format!("failed to resolve node directory: {error}"))?;
    let name = base
        .file_name()
        .ok_or_else(|| "snapshot base requires a file name".to_owned())?;
    let lock_path = append_suffix(&parent.join(name), ".runtime.lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| {
            format!(
                "failed to open node runtime lock {}: {error}",
                lock_path.display()
            )
        })?;
    file.try_lock().map_err(|error| {
        format!(
            "node directory is already in use or cannot be locked {}: {error}",
            lock_path.display()
        )
    })?;
    Ok(file)
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

pub(crate) fn random_signing_key() -> Result<SigningKey, String> {
    let mut seed = [0_u8; 32];
    getrandom::fill(&mut seed)
        .map_err(|error| format!("failed to obtain OS randomness for signing key: {error}"))?;
    Ok(SigningKey::from_bytes(&seed))
}

#[cfg(unix)]
pub(crate) fn validate_private_file_permissions(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;

    let metadata = std::fs::metadata(path).map_err(|error| {
        format!(
            "failed to inspect private key file {} permissions: {error}",
            path.display()
        )
    })?;
    let mode = metadata.permissions().mode();
    if mode & 0o077 != 0 {
        return Err(format!(
            "private key file {} must not be readable or writable by group/other; expected permissions 0600 or stricter",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn validate_private_file_permissions(_path: &Path) -> Result<(), String> {
    Ok(())
}
