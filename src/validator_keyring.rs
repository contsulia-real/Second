use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::SigningKey;
use second::{PersistedNodeState, ValidatorCredential, ValidatorId, ValidatorRuntimeKeys};
use serde::{Deserialize, Serialize};

use crate::local_file::{
    append_suffix, decode_standard_base64_32, read_bounded, write_new_private,
};

const MAX_VALIDATOR_KEYRING_SIZE: usize = 64 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ValidatorKeyringFile {
    validator_id: u64,
    identity_private_key_base64: String,
    recovery_private_key_base64: String,
    consensus_private_keys_base64: Vec<String>,
}

#[derive(Clone)]
pub(crate) struct ValidatorKeyringMaterial {
    validator_id: ValidatorId,
    identity_key: SigningKey,
    recovery_key: SigningKey,
    consensus_keys: Vec<SigningKey>,
}

impl ValidatorKeyringMaterial {
    pub(crate) const fn validator_id(&self) -> ValidatorId {
        self.validator_id
    }

    pub(crate) fn consensus_key_count(&self) -> usize {
        self.consensus_keys.len()
    }

    pub(crate) fn identity_key(&self) -> &SigningKey {
        &self.identity_key
    }

    pub(crate) fn recovery_key(&self) -> &SigningKey {
        &self.recovery_key
    }

    pub(crate) fn consensus_key(&self, index: usize) -> Option<&SigningKey> {
        self.consensus_keys.get(index)
    }

    pub(crate) fn credential(&self) -> Result<ValidatorCredential, String> {
        let consensus_key = self.consensus_keys.first().ok_or_else(|| {
            "validator keyring must contain at least one consensus private key".to_owned()
        })?;
        ValidatorCredential::new(
            self.validator_id,
            self.identity_key.verifying_key().to_bytes(),
            consensus_key.verifying_key().to_bytes(),
            self.recovery_key.verifying_key().to_bytes(),
        )
        .map_err(|error| format!("invalid validator credential material: {error:?}"))
    }
}

pub(crate) fn generate(path: &Path, validator_id: u64) -> Result<ValidatorCredential, String> {
    let material = ValidatorKeyringMaterial {
        validator_id: ValidatorId::new(validator_id),
        identity_key: random_signing_key()?,
        recovery_key: random_signing_key()?,
        consensus_keys: vec![random_signing_key()?],
    };
    let credential = material.credential()?;
    write_material(path, &material)?;
    Ok(credential)
}

pub(crate) fn read_material(path: &Path) -> Result<ValidatorKeyringMaterial, String> {
    validate_private_file_permissions(path)?;
    let bytes = read_bounded(path, MAX_VALIDATOR_KEYRING_SIZE, "validator keyring")?;
    let file = serde_json::from_slice::<ValidatorKeyringFile>(&bytes)
        .map_err(|error| format!("invalid validator keyring {}: {error}", path.display()))?;

    let identity_key = decode_signing_key(
        path,
        "identity_private_key_base64",
        &file.identity_private_key_base64,
    )?;
    let recovery_key = decode_signing_key(
        path,
        "recovery_private_key_base64",
        &file.recovery_private_key_base64,
    )?;
    let consensus_keys = file
        .consensus_private_keys_base64
        .iter()
        .enumerate()
        .map(|(index, value)| {
            decode_signing_key(
                path,
                &format!("consensus_private_keys_base64[{index}]"),
                value,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;

    if consensus_keys.is_empty() {
        return Err(format!(
            "validator keyring {} must contain at least one consensus private key",
            path.display()
        ));
    }

    let mut seen = BTreeSet::new();
    for key in &consensus_keys {
        if !seen.insert(key.verifying_key().to_bytes()) {
            return Err(format!(
                "validator keyring {} contains a duplicate consensus private key",
                path.display()
            ));
        }
    }

    Ok(ValidatorKeyringMaterial {
        validator_id: ValidatorId::new(file.validator_id),
        identity_key,
        recovery_key,
        consensus_keys,
    })
}

pub(crate) fn write_material(
    path: &Path,
    material: &ValidatorKeyringMaterial,
) -> Result<(), String> {
    let file = ValidatorKeyringFile {
        validator_id: material.validator_id.value(),
        identity_private_key_base64: STANDARD.encode(material.identity_key.to_bytes()),
        recovery_private_key_base64: STANDARD.encode(material.recovery_key.to_bytes()),
        consensus_private_keys_base64: material
            .consensus_keys
            .iter()
            .map(|key| STANDARD.encode(key.to_bytes()))
            .collect(),
    };
    let bytes = serde_json::to_vec_pretty(&file)
        .map_err(|error| format!("failed to encode validator keyring: {error}"))?;
    write_new_private(path, &bytes, "validator keyring")
}

pub(crate) fn load_with_additional(
    snapshot_base: &str,
    persisted: &PersistedNodeState,
    additional_consensus_keys: Vec<SigningKey>,
) -> Result<ValidatorRuntimeKeys, String> {
    let path = keyring_path(Path::new(snapshot_base));
    let material = read_material(&path)?;
    let validator_id = material.validator_id();

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

    if material.identity_key.verifying_key().to_bytes() != registry_credential.identity_public_key()
    {
        return Err(format!(
            "validator keyring {} identity key does not match ValidatorId {}",
            path.display(),
            validator_id.value()
        ));
    }

    if material.recovery_key.verifying_key().to_bytes() != registry_credential.recovery_public_key()
    {
        return Err(format!(
            "validator keyring {} recovery key does not match ValidatorId {}",
            path.display(),
            validator_id.value()
        ));
    }

    let mut consensus_keys = BTreeMap::new();
    for (index, key) in material.consensus_keys.into_iter().enumerate() {
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
        consensus_keys.insert(public_key, key);
    }

    for key in additional_consensus_keys {
        let public_key = key.verifying_key().to_bytes();
        if consensus_keys.insert(public_key, key).is_some() {
            return Err(format!(
                "validator keyring {} contains a duplicate consensus private key across base keyring and rotation-key log",
                path.display()
            ));
        }
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
        .expect("non-empty consensus key material checked above");
    let mut runtime_keys = ValidatorRuntimeKeys::new(validator_id, material.identity_key, first);
    for key in consensus_keys {
        runtime_keys = runtime_keys.with_consensus_key(key);
    }
    Ok(runtime_keys)
}

pub(crate) fn keyring_path(snapshot_base: &Path) -> PathBuf {
    append_suffix(snapshot_base, ".validator.keys.json")
}

pub(crate) fn random_signing_key() -> Result<SigningKey, String> {
    let mut seed = [0_u8; 32];
    getrandom::fill(&mut seed)
        .map_err(|error| format!("failed to obtain OS randomness for validator key: {error}"))?;
    Ok(SigningKey::from_bytes(&seed))
}

fn decode_signing_key(path: &Path, field: &str, value: &str) -> Result<SigningKey, String> {
    let bytes = decode_standard_base64_32(value).map_err(|error| {
        format!(
            "invalid validator keyring {} {field}: {error}",
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
