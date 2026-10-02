use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::File;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::SigningKey;
use second::{PersistedNodeState, ValidatorId, ValidatorRuntimeKeys};
use serde::Deserialize;

const MAX_VALIDATOR_KEYRING_SIZE: usize = 64 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ValidatorKeyringFile {
    validator_id: u64,
    identity_private_key_base64: String,
    recovery_private_key_base64: String,
    consensus_private_keys_base64: Vec<String>,
}

pub(crate) fn load(
    snapshot_base: &str,
    persisted: &PersistedNodeState,
) -> Result<ValidatorRuntimeKeys, String> {
    let path = keyring_path(Path::new(snapshot_base));
    validate_private_file_permissions(&path)?;
    let bytes = read_bounded(&path, MAX_VALIDATOR_KEYRING_SIZE, "validator keyring")?;

    let file = serde_json::from_slice::<ValidatorKeyringFile>(&bytes)
        .map_err(|error| format!("invalid validator keyring {}: {error}", path.display()))?;
    let validator_id = ValidatorId::new(file.validator_id);

    let registry_credential = persisted
        .validator_registry
        .credential(validator_id)
        .ok_or_else(|| {
            format!(
                "validator keyring {} references unknown ValidatorId {}",
                path.display(),
                validator_id.value()
            )
        })?;

    let identity_key = decode_signing_key(
        &path,
        "identity_private_key_base64",
        &file.identity_private_key_base64,
    )?;
    if identity_key.verifying_key().to_bytes() != registry_credential.identity_public_key() {
        return Err(format!(
            "validator keyring {} identity key does not match ValidatorId {}",
            path.display(),
            validator_id.value()
        ));
    }

    let recovery_key = decode_signing_key(
        &path,
        "recovery_private_key_base64",
        &file.recovery_private_key_base64,
    )?;
    if recovery_key.verifying_key().to_bytes() != registry_credential.recovery_public_key() {
        return Err(format!(
            "validator keyring {} recovery key does not match ValidatorId {}",
            path.display(),
            validator_id.value()
        ));
    }

    let mut consensus_keys = BTreeMap::new();
    for (index, encoded) in file.consensus_private_keys_base64.iter().enumerate() {
        let key = decode_signing_key(
            &path,
            &format!("consensus_private_keys_base64[{index}]"),
            encoded,
        )?;
        let public_key = key.verifying_key().to_bytes();
        if !persisted
            .validator_registry
            .has_consensus_key_in_history(validator_id, public_key)
        {
            return Err(format!(
                "validator keyring {} consensus key {index} is not in ValidatorId {} history",
                path.display(),
                validator_id.value()
            ));
        }
        if consensus_keys.insert(public_key, key).is_some() {
            return Err(format!(
                "validator keyring {} contains a duplicate consensus private key",
                path.display()
            ));
        }
    }

    if consensus_keys.is_empty() {
        return Err(format!(
            "validator keyring {} must contain at least one consensus private key",
            path.display()
        ));
    }

    let mut required = BTreeSet::new();
    if let Some(credential) = persisted.validator_set.validator(validator_id) {
        required.insert(credential.consensus_public_key());
    }
    for validator_set in persisted.retained_validator_sets.values() {
        if let Some(credential) = validator_set.validator(validator_id) {
            required.insert(credential.consensus_public_key());
        }
    }
    if required.is_empty() {
        return Err(format!(
            "ValidatorId {} has no active or retained validator authority in snapshot",
            validator_id.value()
        ));
    }

    for public_key in &required {
        if !consensus_keys.contains_key(public_key) {
            return Err(format!(
                "validator keyring {} is missing a consensus private key required by an active or retained ValidatorSet",
                path.display()
            ));
        }
    }

    let mut consensus_keys = consensus_keys.into_values();
    let first = consensus_keys
        .next()
        .expect("non-empty consensus key map checked above");
    let mut runtime_keys = ValidatorRuntimeKeys::new(validator_id, identity_key, first);
    for key in consensus_keys {
        runtime_keys = runtime_keys.with_consensus_key(key);
    }
    Ok(runtime_keys)
}

fn read_bounded(path: &Path, maximum: usize, label: &str) -> Result<Vec<u8>, String> {
    let file = File::open(path)
        .map_err(|error| format!("failed to read {label} {}: {error}", path.display()))?;
    let limit = u64::try_from(maximum)
        .expect("validator sidecar size limit must fit u64")
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

pub(crate) fn keyring_path(snapshot_base: &Path) -> PathBuf {
    let mut path = OsString::from(snapshot_base.as_os_str());
    path.push(".validator.keys.json");
    PathBuf::from(path)
}

fn decode_signing_key(path: &Path, field: &str, value: &str) -> Result<SigningKey, String> {
    let decoded = STANDARD.decode(value).map_err(|_| {
        format!(
            "invalid validator keyring {} {field}: must be valid standard base64",
            path.display()
        )
    })?;
    let bytes: [u8; 32] = decoded.try_into().map_err(|_| {
        format!(
            "invalid validator keyring {} {field}: must decode to exactly 32 bytes",
            path.display()
        )
    })?;
    Ok(SigningKey::from_bytes(&bytes))
}

#[cfg(unix)]
fn validate_private_file_permissions(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;

    let metadata = std::fs::metadata(path).map_err(|error| {
        format!(
            "failed to inspect validator keyring {} permissions: {error}",
            path.display()
        )
    })?;
    let mode = metadata.permissions().mode();
    if mode & 0o077 != 0 {
        return Err(format!(
            "validator keyring {} must not be readable or writable by group/other; expected permissions 0600 or stricter",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_private_file_permissions(_path: &Path) -> Result<(), String> {
    Ok(())
}
