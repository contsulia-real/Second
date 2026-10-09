use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};

use crate::local_file;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthorizerKeyFile {
    private_key_base64: String,
}

pub(crate) fn keygen(path: &str) -> Result<(), String> {
    let key = local_file::random_signing_key()?;
    let bytes = serde_json::to_vec_pretty(&AuthorizerKeyFile {
        private_key_base64: STANDARD.encode(key.to_bytes()),
    })
    .map_err(|error| format!("failed to encode authorizer key: {error}"))?;
    local_file::write_new_private(Path::new(path), &bytes, "authorizer key")?;
    println!(
        "AUTHORIZER public_key={}",
        STANDARD.encode(key.verifying_key().to_bytes())
    );
    Ok(())
}

pub(crate) fn sign(key_file: &str, unsigned_file: &str, signed_file: &str) -> Result<(), String> {
    let path = Path::new(key_file);
    local_file::validate_private_file_permissions(path)?;
    let bytes = local_file::read_bounded(path, 4096, "authorizer key")?;
    let material: AuthorizerKeyFile = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid authorizer key: {error}"))?;
    let seed = local_file::decode_standard_base64_32(&material.private_key_base64)
        .map_err(|error| format!("invalid authorizer private key: {error}"))?;
    let key = SigningKey::from_bytes(&seed);
    let input = local_file::read_bounded(
        Path::new(unsigned_file),
        crate::cli_legal_task::MAX_TRANSACTION_REQUEST_JSON_SIZE,
        "unsigned transaction",
    )?;
    let signed = second::sign_transaction_request_json(&input, &key)
        .map_err(|error| format!("invalid unsigned transaction: {error:?}"))?;
    if signed.len() > crate::cli_legal_task::MAX_TRANSACTION_REQUEST_JSON_SIZE {
        return Err("signed transaction exceeds JSON input budget".to_owned());
    }
    local_file::write_new_private(Path::new(signed_file), &signed, "signed transaction")?;
    let task = second::parse_transaction_request_json(&signed, key.verifying_key().to_bytes())
        .map_err(|error| format!("invalid signed transaction: {error:?}"))?;
    println!(
        "SIGNED task={} authorizer={}",
        task.payload().task_id(),
        STANDARD.encode(key.verifying_key().to_bytes())
    );
    Ok(())
}
