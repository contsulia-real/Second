use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use ed25519_dalek::SigningKey;
use second::{
    CURRENT_PROTOCOL_VERSION, PersistedNodeState, ValidatorConsensusKeyRotationRequest,
    ValidatorRotationAuthority,
};

use crate::local_file::append_suffix;

const RECORD_SIZE: usize = 32 + 117;
const MAX_ROTATION_KEY_LOG_SIZE: usize = RECORD_SIZE * 1024;

pub(crate) fn prepare(
    snapshot_base: &Path,
    authority: ValidatorRotationAuthority,
    request_path: &Path,
) -> Result<ValidatorConsensusKeyRotationRequest, String> {
    let store = second::StateStore::new(snapshot_base);
    let persisted = store
        .load()
        .map_err(|error| format!("failed to load validator snapshot: {error:?}"))?
        .ok_or_else(|| format!("no snapshot found at {}", snapshot_base.display()))?;
    let keyring_path = crate::validator_keyring::keyring_path(snapshot_base);
    let material = crate::validator_keyring::read_material(&keyring_path)?;
    let validator_id = material.validator_id();
    let current = persisted
        .validator_set
        .validator(validator_id)
        .ok_or_else(|| {
            format!(
                "ValidatorId {} is not active in ValidatorSet {}",
                validator_id.value(),
                persisted.validator_set.version()
            )
        })?;

    let existing = load_verified(snapshot_base, &persisted, &material)?;
    let future_count = existing
        .iter()
        .filter(|record| {
            !persisted
                .validator_registry
                .has_consensus_key_in_history(validator_id, record.key.verifying_key().to_bytes())
        })
        .count();
    if future_count != 0 {
        return Err(
            "a future consensus-key rotation is already prepared for this validator".to_owned(),
        );
    }

    let new_key = crate::validator_keyring::random_signing_key()?;
    let signing_key = match authority {
        ValidatorRotationAuthority::Identity => material.identity_key(),
        ValidatorRotationAuthority::Recovery => material.recovery_key(),
    };
    let request = ValidatorConsensusKeyRotationRequest::sign(
        CURRENT_PROTOCOL_VERSION,
        authority,
        validator_id,
        persisted.validator_set.version(),
        new_key.verifying_key().to_bytes(),
        signing_key,
    )
    .map_err(|error| format!("failed to sign validator rotation request: {error:?}"))?;
    request
        .verify(current)
        .map_err(|error| format!("generated validator rotation request is invalid: {error:?}"))?;

    append_record(snapshot_base, &new_key, &request)?;
    crate::local_file::write_new(
        request_path,
        &request.encode_bytes(),
        "validator rotation request",
    )?;
    Ok(request)
}

pub(crate) fn load_consensus_keys(
    snapshot_base: &Path,
    persisted: &PersistedNodeState,
) -> Result<Vec<SigningKey>, String> {
    let keyring_path = crate::validator_keyring::keyring_path(snapshot_base);
    let material = crate::validator_keyring::read_material(&keyring_path)?;
    Ok(load_verified(snapshot_base, persisted, &material)?
        .into_iter()
        .map(|record| record.key)
        .collect())
}

struct RotationRecord {
    key: SigningKey,
}

fn load_verified(
    snapshot_base: &Path,
    persisted: &PersistedNodeState,
    material: &crate::validator_keyring::ValidatorKeyringMaterial,
) -> Result<Vec<RotationRecord>, String> {
    let path = rotation_key_log_path(snapshot_base);
    if !path.try_exists().map_err(|error| {
        format!(
            "failed to inspect rotation-key log {}: {error}",
            path.display()
        )
    })? {
        return Ok(Vec::new());
    }
    validate_private_permissions(&path)?;
    let mut file = File::open(&path).map_err(|error| {
        format!(
            "failed to read rotation-key log {}: {error}",
            path.display()
        )
    })?;
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take((MAX_ROTATION_KEY_LOG_SIZE + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            format!(
                "failed to read rotation-key log {}: {error}",
                path.display()
            )
        })?;
    if bytes.len() > MAX_ROTATION_KEY_LOG_SIZE {
        return Err(format!(
            "rotation-key log {} exceeds {} bytes",
            path.display(),
            MAX_ROTATION_KEY_LOG_SIZE
        ));
    }

    let validator_id = material.validator_id();
    let current = persisted
        .validator_registry
        .credential(validator_id)
        .ok_or_else(|| {
            format!(
                "rotation-key log references ValidatorId {} absent from durable registry",
                validator_id.value()
            )
        })?;
    let complete_len = bytes.len() / RECORD_SIZE * RECORD_SIZE;
    let mut records = Vec::with_capacity(complete_len / RECORD_SIZE);
    let mut future_count = 0_usize;
    for chunk in bytes[..complete_len].as_chunks::<RECORD_SIZE>().0 {
        let seed: [u8; 32] = chunk[..32]
            .try_into()
            .expect("fixed rotation record seed length");
        let key = SigningKey::from_bytes(&seed);
        let request =
            ValidatorConsensusKeyRotationRequest::decode_bytes(&chunk[32..]).ok_or_else(|| {
                format!(
                    "rotation-key log {} contains an invalid record",
                    path.display()
                )
            })?;
        if request.validator_id() != validator_id
            || request.new_consensus_public_key() != key.verifying_key().to_bytes()
            || request.current_validator_set_version() > persisted.validator_set.version()
        {
            return Err(format!(
                "rotation-key log {} contains a record inconsistent with current ValidatorId/version",
                path.display()
            ));
        }
        request.verify(current).map_err(|error| {
            format!(
                "rotation-key log {} contains an unauthorized rotation request: {error:?}",
                path.display()
            )
        })?;

        let in_history = persisted
            .validator_registry
            .has_consensus_key_in_history(validator_id, key.verifying_key().to_bytes());
        if !in_history {
            if request.current_validator_set_version() != persisted.validator_set.version() {
                return Err(format!(
                    "rotation-key log {} contains a stale unfinalized future key",
                    path.display()
                ));
            }
            future_count += 1;
            if future_count > 1 {
                return Err(format!(
                    "rotation-key log {} contains more than one future consensus key",
                    path.display()
                ));
            }
        }
        records.push(RotationRecord { key });
    }
    Ok(records)
}

fn append_record(
    snapshot_base: &Path,
    key: &SigningKey,
    request: &ValidatorConsensusKeyRotationRequest,
) -> Result<(), String> {
    let path = rotation_key_log_path(snapshot_base);
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create rotation-key log directory {}: {error}",
                parent.display()
            )
        })?;
    }
    if path.exists() {
        validate_private_permissions(&path)?;
        let len = std::fs::metadata(&path)
            .map_err(|error| {
                format!(
                    "failed to inspect rotation-key log {}: {error}",
                    path.display()
                )
            })?
            .len() as usize;
        if len > MAX_ROTATION_KEY_LOG_SIZE.saturating_sub(RECORD_SIZE) {
            return Err(format!(
                "rotation-key log {} has reached its bounded capacity",
                path.display()
            ));
        }
        if !len.is_multiple_of(RECORD_SIZE) {
            let complete_len = len / RECORD_SIZE * RECORD_SIZE;
            let file = OpenOptions::new()
                .write(true)
                .open(&path)
                .map_err(|error| {
                    format!(
                        "failed to repair rotation-key log {}: {error}",
                        path.display()
                    )
                })?;
            file.set_len(complete_len as u64).map_err(|error| {
                format!(
                    "failed to truncate incomplete rotation-key tail {}: {error}",
                    path.display()
                )
            })?;
            file.sync_all().map_err(|error| {
                format!(
                    "failed to sync repaired rotation-key log {}: {error}",
                    path.display()
                )
            })?;
        }
    }

    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&path).map_err(|error| {
        format!(
            "failed to open rotation-key log {}: {error}",
            path.display()
        )
    })?;
    file.write_all(&key.to_bytes())
        .and_then(|_| file.write_all(&request.encode_bytes()))
        .map_err(|error| {
            format!(
                "failed to append rotation-key log {}: {error}",
                path.display()
            )
        })?;
    file.sync_all().map_err(|error| {
        format!(
            "failed to sync rotation-key log {}: {error}",
            path.display()
        )
    })
}

pub(crate) fn rotation_key_log_path(snapshot_base: &Path) -> PathBuf {
    append_suffix(snapshot_base, ".validator.rotation.keys")
}

#[cfg(unix)]
fn validate_private_permissions(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;

    let mode = std::fs::metadata(path)
        .map_err(|error| {
            format!(
                "failed to inspect rotation-key log {}: {error}",
                path.display()
            )
        })?
        .permissions()
        .mode();
    if mode & 0o077 != 0 {
        return Err(format!(
            "rotation-key log {} must be 0600 or stricter",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_private_permissions(_path: &Path) -> Result<(), String> {
    Ok(())
}
