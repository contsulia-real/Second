use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::VerifyingKey;
use second::{AuthorizerSet, BftTimeoutConfig, CURRENT_PROTOCOL_VERSION, ValidatorRuntimeConfig};
use serde::{Deserialize, Serialize};

use crate::local_file::{append_suffix, decode_standard_base64_32, read_bounded, write_new};

const MAX_VALIDATOR_CONFIG_SIZE: usize = 64 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ValidatorConfigFile {
    authorizer_public_keys_base64: Vec<String>,
    network_id_base64: String,
    bft_timeouts_ms: BftTimeoutFile,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BftTimeoutFile {
    pub(crate) proposal: u64,
    pub(crate) prevote: u64,
    pub(crate) precommit: u64,
}

impl BftTimeoutFile {
    pub(crate) fn validate(self) -> Result<(), String> {
        if self.proposal == 0 {
            return Err("bft_timeouts_ms.proposal must be greater than zero".to_owned());
        }
        if self.prevote == 0 {
            return Err("bft_timeouts_ms.prevote must be greater than zero".to_owned());
        }
        if self.precommit == 0 {
            return Err("bft_timeouts_ms.precommit must be greater than zero".to_owned());
        }
        Ok(())
    }

    fn to_runtime(self) -> Result<BftTimeoutConfig, String> {
        self.validate()?;
        Ok(BftTimeoutConfig::new(
            Duration::from_millis(self.proposal),
            Duration::from_millis(self.prevote),
            Duration::from_millis(self.precommit),
        ))
    }
}

pub(crate) fn load(snapshot_base: &str) -> Result<ValidatorRuntimeConfig, String> {
    let (authorizers, timeouts) = load_parts(snapshot_base)?;
    Ok(ValidatorRuntimeConfig::new(
        authorizers,
        timeouts,
        system_unix_seconds,
    ))
}

pub(crate) fn load_parts(snapshot_base: &str) -> Result<(AuthorizerSet, BftTimeoutConfig), String> {
    let path = config_path(Path::new(snapshot_base));
    let bytes = read_bounded(&path, MAX_VALIDATOR_CONFIG_SIZE, "validator config")?;

    let file = serde_json::from_slice::<ValidatorConfigFile>(&bytes)
        .map_err(|error| format!("invalid validator config {}: {error}", path.display()))?;
    let public_keys = decode_authorizer_keys(&path, &file.authorizer_public_keys_base64)?;
    let network_id = decode_standard_base64_32(&file.network_id_base64).map_err(str::to_owned)?;
    let authorizers =
        AuthorizerSet::new_for_network(CURRENT_PROTOCOL_VERSION, network_id, public_keys)
            .map_err(|error| format!("invalid validator config {}: {error:?}", path.display()))?;
    let timeouts = file
        .bft_timeouts_ms
        .to_runtime()
        .map_err(|error| format!("invalid validator config {}: {error}", path.display()))?;

    Ok((authorizers, timeouts))
}

pub(crate) fn write(
    snapshot_base: &Path,
    authorizer_public_keys: &[[u8; 32]],
    network_id: [u8; 32],
    bft_timeouts_ms: BftTimeoutFile,
) -> Result<(), String> {
    bft_timeouts_ms.validate()?;
    AuthorizerSet::new(
        CURRENT_PROTOCOL_VERSION,
        authorizer_public_keys.iter().copied(),
    )
    .map_err(|error| format!("invalid AuthorizerSet for validator config: {error:?}"))?;

    let file = ValidatorConfigFile {
        network_id_base64: STANDARD.encode(network_id),
        authorizer_public_keys_base64: authorizer_public_keys
            .iter()
            .map(|key| STANDARD.encode(key))
            .collect(),
        bft_timeouts_ms,
    };
    let bytes = serde_json::to_vec_pretty(&file)
        .map_err(|error| format!("failed to encode validator config: {error}"))?;
    write_new(&config_path(snapshot_base), &bytes, "validator config")
}

pub(crate) fn config_path(snapshot_base: &Path) -> PathBuf {
    append_suffix(snapshot_base, ".validator.json")
}

fn decode_authorizer_keys(path: &Path, values: &[String]) -> Result<Vec<[u8; 32]>, String> {
    values
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
        .collect()
}

fn system_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(u64::MAX, |duration| duration.as_secs())
}
