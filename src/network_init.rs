use std::collections::BTreeSet;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::VerifyingKey;
use second::{
    AccountAddress, AuthorizerSet, CURRENT_PROTOCOL_VERSION, MAX_PEER_RECORDS, NodeId, PeerRecord,
    QuicTransportIdentity, SecondState, StateStore, ValidatorId, ValidatorSet,
    transport_identity_path,
};
use serde::Deserialize;

use crate::local_file::{append_suffix, decode_standard_base64_32, read_bounded};
use crate::validator_config::{self, BftTimeoutFile};
use crate::validator_keyring::{self, ValidatorKeyringMaterial};

const MAX_NETWORK_INIT_CONFIG_SIZE: usize = 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NetworkInitFile {
    validator_set_version: u64,
    first_currency_address: u64,
    reserve_count: u64,
    accounts: Vec<String>,
    authorizer_public_keys_base64: Vec<String>,
    bft_timeouts_ms: BftTimeoutFile,
    validators: Vec<ValidatorInitFile>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ValidatorInitFile {
    validator_id: u64,
    listen_address: String,
    keyring_file: String,
}

struct PreparedValidator {
    validator_id: ValidatorId,
    listen_address: SocketAddr,
    keyring: ValidatorKeyringMaterial,
}

pub(crate) struct InitializedNode {
    pub(crate) validator_id: ValidatorId,
    pub(crate) listen_address: SocketAddr,
    pub(crate) snapshot_base: PathBuf,
    pub(crate) node_id: NodeId,
    pub(crate) certificate_der: Vec<u8>,
}

pub(crate) fn init_network(
    config_path: &Path,
    output_dir: &Path,
) -> Result<Vec<InitializedNode>, String> {
    let config_bytes = read_bounded(
        config_path,
        MAX_NETWORK_INIT_CONFIG_SIZE,
        "network init config",
    )?;
    let config = serde_json::from_slice::<NetworkInitFile>(&config_bytes).map_err(|error| {
        format!(
            "invalid network init config {}: {error}",
            config_path.display()
        )
    })?;
    config.bft_timeouts_ms.validate()?;

    let authorizer_keys =
        decode_authorizer_keys(config_path, &config.authorizer_public_keys_base64)?;
    AuthorizerSet::new(CURRENT_PROTOCOL_VERSION, authorizer_keys.iter().copied())
        .map_err(|error| format!("invalid genesis AuthorizerSet: {error:?}"))?;

    let accounts = parse_accounts(config_path, &config.accounts)?;
    let mut state = SecondState::genesis(accounts, config.first_currency_address);
    state = state
        .with_reserve(config.reserve_count)
        .map_err(|error| format!("invalid genesis Reserve allocation: {error:?}"))?;

    let config_parent = config_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut prepared = prepare_validators(config_path, config_parent, config.validators)?;
    prepared.sort_by_key(|validator| validator.validator_id);

    let validator_set = ValidatorSet::new(
        config.validator_set_version,
        prepared
            .iter()
            .map(|validator| validator.keyring.credential())
            .collect::<Result<Vec<_>, _>>()?,
    )
    .map_err(|error| format!("invalid genesis ValidatorSet: {error:?}"))?;

    ensure_new_output(output_dir)?;
    let staging = append_suffix(output_dir, ".new");
    if staging.try_exists().map_err(|error| {
        format!(
            "failed to inspect network init staging directory {}: {error}",
            staging.display()
        )
    })? {
        return Err(format!(
            "network init staging directory {} already exists; remove the stale directory before retrying",
            staging.display()
        ));
    }
    if let Some(parent) = output_dir
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create network output parent {}: {error}",
                parent.display()
            )
        })?;
    }
    fs::create_dir(&staging).map_err(|error| {
        format!(
            "failed to create network init staging directory {}: {error}",
            staging.display()
        )
    })?;

    let build_result = build_staging_network(
        &staging,
        output_dir,
        &prepared,
        &state,
        &validator_set,
        &authorizer_keys,
        config.bft_timeouts_ms,
    );
    let initialized = match build_result {
        Ok(initialized) => initialized,
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
    };

    if let Err(error) = fs::rename(&staging, output_dir) {
        let _ = fs::remove_dir_all(&staging);
        return Err(format!(
            "failed to publish initialized network {}: {error}",
            output_dir.display()
        ));
    }
    Ok(initialized)
}

fn prepare_validators(
    config_path: &Path,
    config_parent: &Path,
    validators: Vec<ValidatorInitFile>,
) -> Result<Vec<PreparedValidator>, String> {
    if validators.is_empty() {
        return Err("network init config must contain at least one validator".to_owned());
    }
    let maximum_validators = usize::from(MAX_PEER_RECORDS) + 1;
    if validators.len() > maximum_validators {
        return Err(format!(
            "init-network currently supports at most {maximum_validators} genesis validators because every validator must bootstrap all other exact-set validators; this is a deployment/runtime discovery limit, not a ValidatorSet protocol limit"
        ));
    }

    let mut ids = BTreeSet::new();
    let mut addresses = BTreeSet::new();
    let mut prepared = Vec::with_capacity(validators.len());
    for (index, validator) in validators.into_iter().enumerate() {
        let validator_id = ValidatorId::new(validator.validator_id);
        if !ids.insert(validator_id) {
            return Err(format!(
                "network init config {} contains duplicate validator_id {}",
                config_path.display(),
                validator.validator_id
            ));
        }

        let listen_address = validator
            .listen_address
            .parse::<SocketAddr>()
            .map_err(|error| {
                format!(
                    "network init config {} validators[{index}].listen_address {:?} is invalid: {error}",
                    config_path.display(),
                    validator.listen_address
                )
            })?;
        PeerRecord::validate_address(listen_address).map_err(|_| {
            format!(
                "network init config {} validators[{index}].listen_address must be a dialable non-wildcard address with a nonzero port",
                config_path.display()
            )
        })?;
        if !addresses.insert(listen_address) {
            return Err(format!(
                "network init config {} contains duplicate listen_address {}",
                config_path.display(),
                listen_address
            ));
        }

        let keyring_path = resolve_path(config_parent, &validator.keyring_file);
        let keyring = validator_keyring::read_material(&keyring_path)?;
        if keyring.validator_id() != validator_id {
            return Err(format!(
                "network init config validator_id {} does not match keyring {} ValidatorId {}",
                validator_id.value(),
                keyring_path.display(),
                keyring.validator_id().value()
            ));
        }
        if keyring.consensus_key_count() != 1 {
            return Err(format!(
                "genesis validator keyring {} must contain exactly one consensus private key; historical consensus keys do not exist at genesis",
                keyring_path.display()
            ));
        }
        keyring.credential().map_err(|error| {
            format!(
                "invalid genesis validator keyring {}: {error}",
                keyring_path.display()
            )
        })?;

        prepared.push(PreparedValidator {
            validator_id,
            listen_address,
            keyring,
        });
    }
    Ok(prepared)
}

fn build_staging_network(
    staging: &Path,
    final_output: &Path,
    validators: &[PreparedValidator],
    state: &SecondState,
    validator_set: &ValidatorSet,
    authorizer_keys: &[[u8; 32]],
    bft_timeouts_ms: BftTimeoutFile,
) -> Result<Vec<InitializedNode>, String> {
    let mut records = Vec::with_capacity(validators.len());
    let mut staged_bases = Vec::with_capacity(validators.len());

    for validator in validators {
        let node_dir = staging.join(format!("validator-{}", validator.validator_id.value()));
        fs::create_dir(&node_dir).map_err(|error| {
            format!(
                "failed to create validator directory {}: {error}",
                node_dir.display()
            )
        })?;
        let snapshot_base = node_dir.join("second");
        StateStore::new(&snapshot_base)
            .initialize(state, validator_set)
            .map_err(|error| {
                format!(
                    "failed to initialize genesis snapshot {}: {error:?}",
                    snapshot_base.display()
                )
            })?;
        validator_keyring::write_material(
            &validator_keyring::keyring_path(&snapshot_base),
            &validator.keyring,
        )?;
        validator_config::write(&snapshot_base, authorizer_keys, bft_timeouts_ms)?;

        let identity =
            QuicTransportIdentity::load_or_generate(transport_identity_path(&snapshot_base))
                .map_err(|error| {
                    format!(
                        "failed to create transport identity for ValidatorId {}: {error:?}",
                        validator.validator_id.value()
                    )
                })?;
        let record = PeerRecord::new(
            identity.node_id(),
            validator.listen_address,
            identity.certificate_der().to_vec(),
        )
        .map_err(|error| {
            format!(
                "failed to create bootstrap record for ValidatorId {}: {error:?}",
                validator.validator_id.value()
            )
        })?;
        records.push(record);
        staged_bases.push(snapshot_base);
    }

    for (index, snapshot_base) in staged_bases.iter().enumerate() {
        let bootstrap = bootstrap_records_for(index, &records);
        crate::bootstrap_config::write(snapshot_base, &bootstrap)?;
    }

    Ok(validators
        .iter()
        .enumerate()
        .map(|(index, validator)| InitializedNode {
            validator_id: validator.validator_id,
            listen_address: validator.listen_address,
            snapshot_base: final_output
                .join(format!("validator-{}", validator.validator_id.value()))
                .join("second"),
            node_id: records[index].node_id(),
            certificate_der: records[index].certificate_der().to_vec(),
        })
        .collect())
}

fn bootstrap_records_for(index: usize, records: &[PeerRecord]) -> Vec<PeerRecord> {
    records
        .iter()
        .enumerate()
        .filter(|(candidate, _)| *candidate != index)
        .map(|(_, record)| record.clone())
        .collect()
}

fn decode_authorizer_keys(path: &Path, values: &[String]) -> Result<Vec<[u8; 32]>, String> {
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let bytes = decode_standard_base64_32(value).map_err(|error| {
                format!(
                    "network init config {} authorizer_public_keys_base64[{index}]: {error}",
                    path.display()
                )
            })?;
            VerifyingKey::from_bytes(&bytes).map_err(|_| {
                format!(
                    "network init config {} authorizer_public_keys_base64[{index}] is not a valid Ed25519 public key",
                    path.display()
                )
            })?;
            Ok(bytes)
        })
        .collect()
}

fn parse_accounts(path: &Path, values: &[String]) -> Result<Vec<AccountAddress>, String> {
    let mut seen = BTreeSet::new();
    let mut accounts = Vec::with_capacity(values.len());
    for (index, value) in values.iter().enumerate() {
        let account = AccountAddress::parse(value).map_err(|error| {
            format!(
                "network init config {} accounts[{index}] {:?} is invalid: {error:?}",
                path.display(),
                value
            )
        })?;
        if !seen.insert(account) {
            return Err(format!(
                "network init config {} contains duplicate account {}",
                path.display(),
                account
            ));
        }
        accounts.push(account);
    }
    Ok(accounts)
}

fn resolve_path(parent: &Path, value: &str) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        parent.join(path)
    }
}

fn ensure_new_output(output_dir: &Path) -> Result<(), String> {
    if output_dir.try_exists().map_err(|error| {
        format!(
            "failed to inspect network output directory {}: {error}",
            output_dir.display()
        )
    })? {
        Err(format!(
            "network output directory {} already exists; init-network never overwrites an existing deployment",
            output_dir.display()
        ))
    } else {
        Ok(())
    }
}

pub(crate) fn certificate_base64(node: &InitializedNode) -> String {
    STANDARD.encode(&node.certificate_der)
}
