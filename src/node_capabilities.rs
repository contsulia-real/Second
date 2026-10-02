use std::path::Path;

use second::{NodeRuntimeCapabilities, PersistedNodeState, ValidatorId};

pub(crate) struct LoadedNodeCapabilities {
    pub(crate) runtime: NodeRuntimeCapabilities,
    pub(crate) validator_id: Option<ValidatorId>,
}

pub(crate) fn load(
    snapshot_base: &str,
    persisted: &PersistedNodeState,
) -> Result<LoadedNodeCapabilities, String> {
    let base = Path::new(snapshot_base);
    let config_path = crate::validator_config::config_path(base);
    let keyring_path = crate::validator_keyring::keyring_path(base);
    let config_present = sidecar_exists(&config_path, "validator config")?;
    let keyring_present = sidecar_exists(&keyring_path, "validator keyring")?;

    match (config_present, keyring_present) {
        (false, false) => Ok(LoadedNodeCapabilities {
            runtime: NodeRuntimeCapabilities::default(),
            validator_id: None,
        }),
        (true, true) => {
            let config = crate::validator_config::load(snapshot_base)?;
            let keys = crate::validator_keyring::load(snapshot_base, persisted)?;
            let validator_id = keys.validator_id();
            Ok(LoadedNodeCapabilities {
                runtime: NodeRuntimeCapabilities::default().with_validator(keys, config),
                validator_id: Some(validator_id),
            })
        }
        _ => Err(format!(
            "validator capability is partially configured: {} and {} must either both exist or both be absent",
            config_path.display(),
            keyring_path.display()
        )),
    }
}

fn sidecar_exists(path: &Path, label: &str) -> Result<bool, String> {
    path.try_exists()
        .map_err(|error| format!("failed to inspect {label} {}: {error}", path.display()))
}
