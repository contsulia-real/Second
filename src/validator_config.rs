use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ed25519_dalek::VerifyingKey;
use second::{AuthorizerSet, BftTimeoutConfig, CURRENT_PROTOCOL_VERSION, ValidatorRuntimeConfig};
use serde::Deserialize;

use crate::local_file::{decode_standard_base64_32, read_bounded};

const MAX_VALIDATOR_CONFIG_SIZE: usize = 64 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ValidatorConfigFile {
    authorizer_public_keys_base64: Vec<String>,
    bft_timeouts_ms: BftTimeoutFile,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BftTimeoutFile {
    proposal: u64,
    prevote: u64,
    precommit: u64,
}

pub(crate) fn load(snapshot_base: &str) -> Result<ValidatorRuntimeConfig, String> {
    let path = config_path(Path::new(snapshot_base));
    let bytes = read_bounded(&path, MAX_VALIDATOR_CONFIG_SIZE, "validator config")?;

    let file = serde_json::from_slice::<ValidatorConfigFile>(&bytes)
        .map_err(|error| format!("invalid validator config {}: {error}", path.display()))?;

    let public_keys = file
        .authorizer_public_keys_base64
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let bytes = decode_standard_base64_32(value).map_err(|error| {
                format!(
                    "invalid validator config {} authorizer_public_keys_base64[{index}]: {error}",
                    path.display()
                )
            })?;
            VerifyingKey::from_bytes(&bytes).map_err(|_| {
                format!(
                    "invalid validator config {} authorizer_public_keys_base64[{index}]: not a valid Ed25519 public key",
                    path.display()
                )
            })?;
            Ok(bytes)
        })
        .collect::<Result<Vec<_>, String>>()?;

    let authorizers = AuthorizerSet::new(CURRENT_PROTOCOL_VERSION, public_keys)
        .map_err(|error| format!("invalid validator config {}: {error:?}", path.display()))?;
    let timeouts = BftTimeoutConfig::new(
        nonzero_duration(&path, "proposal", file.bft_timeouts_ms.proposal)?,
        nonzero_duration(&path, "prevote", file.bft_timeouts_ms.prevote)?,
        nonzero_duration(&path, "precommit", file.bft_timeouts_ms.precommit)?,
    );

    Ok(ValidatorRuntimeConfig::new(
        authorizers,
        timeouts,
        system_unix_seconds,
    ))
}

pub(crate) fn config_path(snapshot_base: &Path) -> PathBuf {
    let mut path = OsString::from(snapshot_base.as_os_str());
    path.push(".validator.json");
    PathBuf::from(path)
}

fn nonzero_duration(path: &Path, field: &str, milliseconds: u64) -> Result<Duration, String> {
    if milliseconds == 0 {
        return Err(format!(
            "invalid validator config {} bft_timeouts_ms.{field}: must be greater than zero",
            path.display()
        ));
    }
    Ok(Duration::from_millis(milliseconds))
}

fn system_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(u64::MAX, |duration| duration.as_secs())
}
