use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use second::{
    CURRENT_PROTOCOL_VERSION, MAX_VALIDATOR_TRANSITION_SOURCE_SIZE, StateStore,
    ValidatorAdmissionRequest, ValidatorConsensusKeyRotationRequest, ValidatorCredential,
    ValidatorId, ValidatorRotationAuthority, ValidatorSet, ValidatorSetTransitionSource,
    client_fetch_state_recovery, client_submit_recovery_checkpoint,
    client_submit_validator_transition,
};
use serde::Deserialize;

const MAX_TRANSITION_PLAN_SIZE: usize = 256 * 1024;
const ADMISSION_REQUEST_SIZE: usize = 300;
const ROTATION_REQUEST_SIZE: usize = 117;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ValidatorTransitionPlan {
    #[serde(default)]
    remove_validator_ids: Vec<u64>,
    #[serde(default)]
    admission_request_files: Vec<String>,
    #[serde(default)]
    rotation_request_files: Vec<String>,
}

pub(crate) fn create_admission(keyring_file: &str, request_file: &str) -> Result<(), String> {
    let material = crate::validator_keyring::read_material(Path::new(keyring_file))?;
    if material.consensus_key_count() != 1 {
        return Err(
            "validator admission requires a fresh keyring with exactly one consensus key"
                .to_owned(),
        );
    }
    let credential = material.credential()?;
    let consensus_key = material
        .consensus_key(0)
        .ok_or_else(|| "validator keyring contains no consensus key".to_owned())?;
    let request = ValidatorAdmissionRequest::sign(
        CURRENT_PROTOCOL_VERSION,
        credential.clone(),
        material.identity_key(),
        consensus_key,
        material.recovery_key(),
    )
    .map_err(|error| format!("failed to sign validator admission request: {error:?}"))?;
    request
        .verify()
        .map_err(|error| format!("generated validator admission request is invalid: {error:?}"))?;
    crate::local_file::write_new(
        Path::new(request_file),
        &request.encode_bytes(),
        "validator admission request",
    )?;

    println!(
        "VALIDATOR-ADMISSION validator={} identity={} consensus={} recovery={} request={}",
        credential.id().value(),
        STANDARD.encode(credential.identity_public_key()),
        STANDARD.encode(credential.consensus_public_key()),
        STANDARD.encode(credential.recovery_public_key()),
        request_file,
    );
    Ok(())
}

pub(crate) fn prepare_rotation(
    snapshot_base: &str,
    authority: &str,
    request_file: &str,
) -> Result<(), String> {
    let authority = parse_rotation_authority(authority)?;
    let request = crate::validator_rotation_keys::prepare(
        Path::new(snapshot_base),
        authority,
        Path::new(request_file),
    )?;
    println!(
        "VALIDATOR-ROTATION validator={} current_set={} authority={} new_consensus={} request={}",
        request.validator_id().value(),
        request.current_validator_set_version(),
        rotation_authority_name(request.authority()),
        STANDARD.encode(request.new_consensus_public_key()),
        request_file,
    );
    Ok(())
}

pub(crate) fn build_transition(
    snapshot_base: &str,
    plan_file: &str,
    source_file: &str,
) -> Result<(), String> {
    let store = StateStore::new(snapshot_base);
    let persisted = store
        .load()
        .map_err(|error| format!("failed to load validator snapshot: {error:?}"))?
        .ok_or_else(|| format!("no snapshot found at {snapshot_base}"))?;
    let plan_bytes = crate::local_file::read_bounded(
        Path::new(plan_file),
        MAX_TRANSITION_PLAN_SIZE,
        "validator transition plan",
    )?;
    let plan = serde_json::from_slice::<ValidatorTransitionPlan>(&plan_bytes)
        .map_err(|error| format!("invalid validator transition plan {plan_file}: {error}"))?;
    let plan_dir = Path::new(plan_file)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    let removals = parse_removals(&plan.remove_validator_ids, &persisted.validator_set)?;
    let admissions = load_admissions(plan_dir, &plan.admission_request_files)?;
    let rotations = load_rotations(plan_dir, &plan.rotation_request_files)?;

    let mut next = persisted
        .validator_set
        .credentials()
        .filter(|credential| !removals.contains(&credential.id()))
        .map(|credential| (credential.id(), credential.clone()))
        .collect::<BTreeMap<_, _>>();

    for request in &rotations {
        let validator_id = request.validator_id();
        let current = persisted
            .validator_set
            .validator(validator_id)
            .ok_or_else(|| {
                format!(
                    "rotation request references ValidatorId {} outside the current ValidatorSet",
                    validator_id.value()
                )
            })?;
        if removals.contains(&validator_id) {
            return Err(format!(
                "ValidatorId {} cannot be removed and rotated in the same transition",
                validator_id.value()
            ));
        }
        let replacement = ValidatorCredential::new(
            validator_id,
            current.identity_public_key(),
            request.new_consensus_public_key(),
            current.recovery_public_key(),
        )
        .map_err(|error| {
            format!(
                "rotation for ValidatorId {} produces an invalid credential: {error:?}",
                validator_id.value()
            )
        })?;
        next.insert(validator_id, replacement);
    }

    for request in &admissions {
        let credential = request.credential().clone();
        if next.insert(credential.id(), credential.clone()).is_some()
            || persisted.validator_registry.contains(credential.id())
        {
            return Err(format!(
                "admission request reuses ValidatorId {}",
                credential.id().value()
            ));
        }
    }

    let next_version = persisted
        .validator_set
        .version()
        .checked_add(1)
        .ok_or_else(|| "ValidatorSet version overflow".to_owned())?;
    let next_validator_set = ValidatorSet::new(next_version, next.into_values())
        .map_err(|error| format!("invalid next ValidatorSet: {error:?}"))?;
    let source = ValidatorSetTransitionSource::new(next_validator_set, admissions, rotations);
    let transition = source
        .verify(&persisted.validator_set, &persisted.validator_registry)
        .map_err(|error| format!("validator transition plan is invalid: {error:?}"))?;
    let bytes = source
        .encode_bytes()
        .map_err(|error| format!("failed to encode validator transition source: {error:?}"))?;
    crate::local_file::write_new(
        Path::new(source_file),
        &bytes,
        "validator transition source",
    )?;

    println!(
        "VALIDATOR-TRANSITION current_set={} next_set={} validators={} digest={} source={}",
        transition.current_validator_set_version(),
        transition.next_validator_set().version(),
        transition.next_validator_set().len(),
        crate::hex_digest(&transition.digest()),
        source_file,
    );
    Ok(())
}

pub(crate) async fn submit_transition(
    address: &str,
    snapshot_base: &str,
    source_file: &str,
    server_certificate: &str,
) -> Result<(), String> {
    let store = StateStore::new(snapshot_base);
    let persisted = store
        .load()
        .map_err(|error| format!("failed to load operator snapshot: {error:?}"))?
        .ok_or_else(|| format!("no snapshot found at {snapshot_base}"))?;
    let material = crate::validator_keyring::read_material(
        &crate::validator_keyring::keyring_path(Path::new(snapshot_base)),
    )?;
    validate_operator_identity(&persisted.validator_set, &material)?;

    let source_bytes = crate::local_file::read_bounded(
        Path::new(source_file),
        MAX_VALIDATOR_TRANSITION_SOURCE_SIZE,
        "validator transition source",
    )?;
    let source = ValidatorSetTransitionSource::decode_bytes(&source_bytes)
        .map_err(|error| format!("invalid validator transition source: {error:?}"))?;
    source
        .verify(&persisted.validator_set, &persisted.validator_registry)
        .map_err(|error| format!("validator transition source is invalid locally: {error:?}"))?;

    let client = crate::quic_client(server_certificate)?;
    let peer = client
        .connect(crate::parse_socket_address(address)?)
        .await
        .map_err(|error| format!("failed to connect QUIC peer {address}: {error:?}"))?;
    let result = client_submit_validator_transition(
        &peer,
        material.validator_id(),
        material.identity_key(),
        persisted.validator_set.version(),
        &source,
    )
    .await;
    peer.close();
    client.wait_idle().await;

    let accepted =
        result.map_err(|error| format!("validator transition submission failed: {error:?}"))?;
    println!(
        "TRANSITION-ACCEPTED current_set={} next_set={} digest={}",
        accepted.current_validator_set_version,
        accepted.next_validator_set_version,
        crate::hex_digest(&accepted.transition_digest),
    );
    Ok(())
}

pub(crate) async fn request_recovery_checkpoint(
    address: &str,
    snapshot_base: &str,
    server_certificate: &str,
) -> Result<(), String> {
    let store = StateStore::new(snapshot_base);
    let persisted = store
        .load()
        .map_err(|error| format!("failed to load operator snapshot: {error:?}"))?
        .ok_or_else(|| format!("no snapshot found at {snapshot_base}"))?;
    let material = crate::validator_keyring::read_material(
        &crate::validator_keyring::keyring_path(Path::new(snapshot_base)),
    )?;
    validate_operator_identity(&persisted.validator_set, &material)?;

    let client = crate::quic_client(server_certificate)?;
    let peer = client
        .connect(crate::parse_socket_address(address)?)
        .await
        .map_err(|error| format!("failed to connect QUIC peer {address}: {error:?}"))?;
    let result = client_submit_recovery_checkpoint(
        &peer,
        material.validator_id(),
        material.identity_key(),
        persisted.validator_set.version(),
    )
    .await;
    peer.close();
    client.wait_idle().await;

    let accepted =
        result.map_err(|error| format!("recovery checkpoint submission failed: {error:?}"))?;
    println!(
        "RECOVERY-CHECKPOINT-ACCEPTED validator_set={} serial={} digest={}",
        accepted.validator_set_version,
        accepted.serial,
        crate::hex_digest(&accepted.checkpoint_digest),
    );
    Ok(())
}

pub(crate) async fn install_recovery(
    address: &str,
    destination_snapshot_base: &str,
    trust_snapshot_base: &str,
    server_certificate: &str,
) -> Result<(), String> {
    let destination = StateStore::new(destination_snapshot_base);
    if destination
        .load()
        .map_err(|error| format!("failed to inspect destination snapshot: {error:?}"))?
        .is_some()
    {
        return Err(format!(
            "destination snapshot {destination_snapshot_base} is already initialized"
        ));
    }

    let trusted_store = StateStore::new(trust_snapshot_base);
    let trusted = trusted_store
        .load()
        .map_err(|error| format!("failed to load trust snapshot: {error:?}"))?
        .ok_or_else(|| format!("no trust snapshot found at {trust_snapshot_base}"))?;
    let keyring_path = crate::validator_keyring::keyring_path(Path::new(destination_snapshot_base));
    let material = crate::validator_keyring::read_material(&keyring_path)?;
    validate_operator_identity(&trusted.validator_set, &material)?;

    let client = crate::quic_client(server_certificate)?;
    let peer = client
        .connect(crate::parse_socket_address(address)?)
        .await
        .map_err(|error| format!("failed to connect QUIC peer {address}: {error:?}"))?;
    let result = client_fetch_state_recovery(
        &peer,
        material.validator_id(),
        material.identity_key(),
        &trusted.validator_set,
    )
    .await;
    peer.close();
    client.wait_idle().await;

    let recovered = result.map_err(|error| format!("state recovery fetch failed: {error:?}"))?;
    destination
        .install_recovered_state(
            &recovered.payload,
            &recovered.checkpoint,
            &trusted.validator_set,
        )
        .map_err(|error| format!("failed to install recovered state: {error:?}"))?;

    println!(
        "RECOVERY-INSTALLED validator={} validator_set={} serial={} safety=locked snapshot={}",
        material.validator_id().value(),
        recovered.checkpoint.checkpoint().validator_set_version(),
        recovered.checkpoint.checkpoint().serial(),
        destination_snapshot_base,
    );
    Ok(())
}

fn load_admissions(
    plan_dir: &Path,
    paths: &[String],
) -> Result<Vec<ValidatorAdmissionRequest>, String> {
    let mut by_id = BTreeMap::new();
    for value in paths {
        let path = resolve_plan_path(plan_dir, value);
        let bytes = crate::local_file::read_bounded(
            &path,
            ADMISSION_REQUEST_SIZE,
            "validator admission request",
        )?;
        let request = ValidatorAdmissionRequest::decode_bytes(&bytes)
            .ok_or_else(|| format!("invalid validator admission request {}", path.display()))?;
        request.verify().map_err(|error| {
            format!(
                "invalid validator admission request {}: {error:?}",
                path.display()
            )
        })?;
        let validator_id = request.credential().id();
        if by_id.insert(validator_id, request).is_some() {
            return Err(format!(
                "duplicate admission request for ValidatorId {}",
                validator_id.value()
            ));
        }
    }
    Ok(by_id.into_values().collect())
}

fn load_rotations(
    plan_dir: &Path,
    paths: &[String],
) -> Result<Vec<ValidatorConsensusKeyRotationRequest>, String> {
    let mut by_id = BTreeMap::new();
    for value in paths {
        let path = resolve_plan_path(plan_dir, value);
        let bytes = crate::local_file::read_bounded(
            &path,
            ROTATION_REQUEST_SIZE,
            "validator rotation request",
        )?;
        let request = ValidatorConsensusKeyRotationRequest::decode_bytes(&bytes)
            .ok_or_else(|| format!("invalid validator rotation request {}", path.display()))?;
        let validator_id = request.validator_id();
        if by_id.insert(validator_id, request).is_some() {
            return Err(format!(
                "duplicate rotation request for ValidatorId {}",
                validator_id.value()
            ));
        }
    }
    Ok(by_id.into_values().collect())
}

fn parse_removals(values: &[u64], current: &ValidatorSet) -> Result<BTreeSet<ValidatorId>, String> {
    let mut removals = BTreeSet::new();
    for value in values {
        let validator_id = ValidatorId::new(*value);
        if !current.contains(validator_id) {
            return Err(format!(
                "cannot remove unknown ValidatorId {} from current ValidatorSet",
                value
            ));
        }
        if !removals.insert(validator_id) {
            return Err(format!(
                "duplicate ValidatorId {} in remove_validator_ids",
                value
            ));
        }
    }
    Ok(removals)
}

fn validate_operator_identity(
    validator_set: &ValidatorSet,
    material: &crate::validator_keyring::ValidatorKeyringMaterial,
) -> Result<(), String> {
    let validator_id = material.validator_id();
    let credential = validator_set.validator(validator_id).ok_or_else(|| {
        format!(
            "ValidatorId {} is not active in ValidatorSet {}",
            validator_id.value(),
            validator_set.version()
        )
    })?;
    if credential.identity_public_key() != material.identity_key().verifying_key().to_bytes() {
        return Err(format!(
            "validator identity key does not match ValidatorId {} in ValidatorSet {}",
            validator_id.value(),
            validator_set.version()
        ));
    }
    Ok(())
}

fn resolve_plan_path(plan_dir: &Path, value: &str) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        plan_dir.join(path)
    }
}

fn parse_rotation_authority(value: &str) -> Result<ValidatorRotationAuthority, String> {
    match value {
        "identity" => Ok(ValidatorRotationAuthority::Identity),
        "recovery" => Ok(ValidatorRotationAuthority::Recovery),
        _ => Err(format!(
            "invalid rotation authority {value:?}; expected identity or recovery"
        )),
    }
}

const fn rotation_authority_name(authority: ValidatorRotationAuthority) -> &'static str {
    match authority {
        ValidatorRotationAuthority::Identity => "identity",
        ValidatorRotationAuthority::Recovery => "recovery",
    }
}
