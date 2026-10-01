use std::sync::{Arc, RwLock};

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

use crate::{
    CURRENT_NETWORK_PROTOCOL_VERSION, CertifiedStateRecoveryCheckpoint, NetworkError, SecondState,
    StateRecoveryPayload, ValidatorId, ValidatorRegistry, ValidatorSet,
};

use super::{NetworkMessage, QuicPeer, RemoteStateRecoveryPayload};

const STATE_RECOVERY_AUTH_DOMAIN: &[u8] = b"SECOND_STATE_RECOVERY_AUTH_V1\0";
const MAX_STATE_RECOVERY_PAYLOAD_SIZE: u64 = 512 * 1024 * 1024;
pub const MAX_STATE_RECOVERY_CHUNK_SIZE: u32 = 60 * 1024;

pub(crate) type StateRecoveryProviderHandle = Arc<RwLock<Option<Arc<StateRecoveryProvider>>>>;

pub(crate) fn new_state_recovery_provider_handle() -> StateRecoveryProviderHandle {
    Arc::new(RwLock::new(None))
}

pub(crate) struct StateRecoveryProvider {
    proof: crate::StateRecoveryCheckpointProof,
    checkpoint_digest: [u8; 32],
    payload: Vec<u8>,
}

struct StateRecoveryRequestContext<'a> {
    state: &'a SecondState,
    validator_set: &'a ValidatorSet,
    validator_registry: &'a ValidatorRegistry,
    validator_id: ValidatorId,
    signature: [u8; 64],
}

impl StateRecoveryProvider {
    pub(crate) fn new(
        state: &SecondState,
        validator_set: &ValidatorSet,
        validator_registry: &ValidatorRegistry,
        checkpoint: &CertifiedStateRecoveryCheckpoint,
    ) -> Result<Self, NetworkError> {
        let payload =
            StateRecoveryPayload::from_shared_parts(state, validator_set, validator_registry)
                .map_err(|_| NetworkError::InvalidStateRecoveryPayload)?;
        checkpoint
            .verify_payload(&payload, validator_set)
            .map_err(|_| NetworkError::InvalidStateRecoveryPayload)?;
        let encoded = payload
            .encode_bytes()
            .map_err(|_| NetworkError::InvalidStateRecoveryPayload)?;
        let payload_len = u64::try_from(encoded.len()).map_err(|_| {
            NetworkError::StateRecoveryPayloadTooLarge {
                announced: u64::MAX,
                maximum: MAX_STATE_RECOVERY_PAYLOAD_SIZE,
            }
        })?;
        if payload_len > MAX_STATE_RECOVERY_PAYLOAD_SIZE {
            return Err(NetworkError::StateRecoveryPayloadTooLarge {
                announced: payload_len,
                maximum: MAX_STATE_RECOVERY_PAYLOAD_SIZE,
            });
        }

        Ok(Self {
            proof: checkpoint.to_unverified_proof(),
            checkpoint_digest: checkpoint.checkpoint().digest(),
            payload: encoded,
        })
    }

    fn matches_shared_state(
        &self,
        state: &SecondState,
        validator_set: &ValidatorSet,
        validator_registry: &ValidatorRegistry,
    ) -> Result<bool, NetworkError> {
        let payload =
            StateRecoveryPayload::from_shared_parts(state, validator_set, validator_registry)
                .map_err(|_| NetworkError::InvalidStateRecoveryPayload)?;
        self.proof
            .checkpoint()
            .matches_payload(&payload)
            .map_err(|_| NetworkError::InvalidStateRecoveryPayload)
    }

    fn manifest_response(
        &self,
        peer: &QuicPeer,
        auth: StateRecoveryRequestContext<'_>,
    ) -> Result<NetworkMessage, NetworkError> {
        let binding = peer.channel_binding()?;
        let message = manifest_auth_bytes(binding, auth.validator_id);
        if !verify_identity_signature(
            auth.validator_set,
            auth.validator_id,
            auth.signature,
            &message,
        ) {
            return Ok(NetworkMessage::StateRecoveryDenied);
        }
        if !self.matches_shared_state(auth.state, auth.validator_set, auth.validator_registry)? {
            return Ok(NetworkMessage::NoStateRecoveryCheckpoint);
        }

        Ok(NetworkMessage::StateRecoveryManifest {
            proof: self.proof.clone(),
            payload_len: self.payload.len() as u64,
        })
    }

    fn chunk_response(
        &self,
        peer: &QuicPeer,
        auth: StateRecoveryRequestContext<'_>,
        checkpoint_digest: [u8; 32],
        offset: u64,
        limit: u32,
    ) -> Result<NetworkMessage, NetworkError> {
        validate_chunk_limit(limit)?;
        let binding = peer.channel_binding()?;
        let message =
            chunk_auth_bytes(binding, auth.validator_id, checkpoint_digest, offset, limit);
        if !verify_identity_signature(
            auth.validator_set,
            auth.validator_id,
            auth.signature,
            &message,
        ) {
            return Ok(NetworkMessage::StateRecoveryDenied);
        }
        if !self.matches_shared_state(auth.state, auth.validator_set, auth.validator_registry)? {
            return Ok(NetworkMessage::NoStateRecoveryCheckpoint);
        }
        if checkpoint_digest != self.checkpoint_digest {
            return Err(NetworkError::InvalidStateRecoveryChunk);
        }

        let start = usize::try_from(offset).map_err(|_| NetworkError::InvalidStateRecoveryChunk)?;
        if start >= self.payload.len() {
            return Err(NetworkError::InvalidStateRecoveryChunk);
        }
        let end = start.saturating_add(limit as usize).min(self.payload.len());

        Ok(NetworkMessage::StateRecoveryChunk {
            checkpoint_digest,
            offset,
            bytes: self.payload[start..end].to_vec(),
        })
    }
}

pub(crate) fn state_recovery_response<F>(
    peer: &QuicPeer,
    message: &NetworkMessage,
    handle: &StateRecoveryProviderHandle,
    load_current_shared_state: F,
) -> Option<Result<NetworkMessage, NetworkError>>
where
    F: FnOnce() -> Result<(SecondState, ValidatorSet, ValidatorRegistry), NetworkError>,
{
    match message {
        NetworkMessage::GetStateRecoveryManifest {
            validator_id,
            signature,
        } => {
            let provider = handle
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            Some(match provider {
                Some(provider) => {
                    load_current_shared_state().and_then(|(state, validator_set, registry)| {
                        provider.manifest_response(
                            peer,
                            StateRecoveryRequestContext {
                                state: &state,
                                validator_set: &validator_set,
                                validator_registry: &registry,
                                validator_id: *validator_id,
                                signature: *signature,
                            },
                        )
                    })
                }
                None => Ok(NetworkMessage::NoStateRecoveryCheckpoint),
            })
        }
        NetworkMessage::GetStateRecoveryChunk {
            validator_id,
            checkpoint_digest,
            offset,
            limit,
            signature,
        } => {
            let provider = handle
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            Some(match provider {
                Some(provider) => {
                    load_current_shared_state().and_then(|(state, validator_set, registry)| {
                        provider.chunk_response(
                            peer,
                            StateRecoveryRequestContext {
                                state: &state,
                                validator_set: &validator_set,
                                validator_registry: &registry,
                                validator_id: *validator_id,
                                signature: *signature,
                            },
                            *checkpoint_digest,
                            *offset,
                            *limit,
                        )
                    })
                }
                None => Ok(NetworkMessage::NoStateRecoveryCheckpoint),
            })
        }
        _ => None,
    }
}

pub async fn client_fetch_state_recovery(
    peer: &QuicPeer,
    validator_id: ValidatorId,
    identity_key: &SigningKey,
    trusted_validator_set: &ValidatorSet,
) -> Result<RemoteStateRecoveryPayload, NetworkError> {
    let binding = peer.channel_binding()?;
    let signature = identity_key
        .sign(&manifest_auth_bytes(binding, validator_id))
        .to_bytes();

    let (proof, payload_len) = match peer
        .exchange(&NetworkMessage::GetStateRecoveryManifest {
            validator_id,
            signature,
        })
        .await?
    {
        NetworkMessage::StateRecoveryManifest { proof, payload_len } => (proof, payload_len),
        NetworkMessage::StateRecoveryDenied => {
            return Err(NetworkError::StateRecoveryUnauthorized);
        }
        NetworkMessage::NoStateRecoveryCheckpoint => {
            return Err(NetworkError::MissingStateRecoveryCheckpoint);
        }
        _ => return Err(NetworkError::UnexpectedMessage),
    };

    if payload_len > MAX_STATE_RECOVERY_PAYLOAD_SIZE {
        return Err(NetworkError::StateRecoveryPayloadTooLarge {
            announced: payload_len,
            maximum: MAX_STATE_RECOVERY_PAYLOAD_SIZE,
        });
    }

    let checkpoint = proof
        .verify_checkpoint(trusted_validator_set)
        .map_err(NetworkError::StateRecoveryFinality)?;
    let checkpoint_digest = checkpoint.checkpoint().digest();
    let target_len =
        usize::try_from(payload_len).map_err(|_| NetworkError::StateRecoveryPayloadTooLarge {
            announced: payload_len,
            maximum: MAX_STATE_RECOVERY_PAYLOAD_SIZE,
        })?;
    let mut encoded = Vec::new();
    encoded.try_reserve_exact(target_len).map_err(|_| {
        NetworkError::StateRecoveryPayloadTooLarge {
            announced: payload_len,
            maximum: MAX_STATE_RECOVERY_PAYLOAD_SIZE,
        }
    })?;

    while encoded.len() < target_len {
        let remaining = target_len - encoded.len();
        let limit = remaining.min(MAX_STATE_RECOVERY_CHUNK_SIZE as usize) as u32;
        let offset = encoded.len() as u64;
        let signature = identity_key
            .sign(&chunk_auth_bytes(
                binding,
                validator_id,
                checkpoint_digest,
                offset,
                limit,
            ))
            .to_bytes();

        let response = peer
            .exchange(&NetworkMessage::GetStateRecoveryChunk {
                validator_id,
                checkpoint_digest,
                offset,
                limit,
                signature,
            })
            .await?;

        let bytes = match response {
            NetworkMessage::StateRecoveryChunk {
                checkpoint_digest: actual_digest,
                offset: actual_offset,
                bytes,
            } if actual_digest == checkpoint_digest && actual_offset == offset => bytes,
            NetworkMessage::StateRecoveryDenied => {
                return Err(NetworkError::StateRecoveryUnauthorized);
            }
            NetworkMessage::NoStateRecoveryCheckpoint => {
                return Err(NetworkError::MissingStateRecoveryCheckpoint);
            }
            _ => return Err(NetworkError::InvalidStateRecoveryChunk),
        };

        if bytes.is_empty() || bytes.len() > limit as usize || bytes.len() > remaining {
            return Err(NetworkError::InvalidStateRecoveryChunk);
        }
        encoded.extend_from_slice(&bytes);
    }

    if encoded.len() != target_len {
        return Err(NetworkError::InvalidStateRecoveryChunk);
    }

    let payload = StateRecoveryPayload::decode_bytes(&encoded)
        .map_err(|_| NetworkError::InvalidStateRecoveryPayload)?;
    checkpoint
        .verify_payload(&payload, trusted_validator_set)
        .map_err(|_| NetworkError::InvalidStateRecoveryPayload)?;

    Ok(RemoteStateRecoveryPayload {
        remote_node_id: peer.remote_node_id(),
        payload,
        checkpoint,
        encoded_payload_len: encoded.len(),
    })
}

pub(crate) fn validate_chunk_limit(limit: u32) -> Result<(), NetworkError> {
    if limit == 0 || limit > MAX_STATE_RECOVERY_CHUNK_SIZE {
        Err(NetworkError::InvalidStateRecoveryChunkLimit {
            requested: limit,
            maximum: MAX_STATE_RECOVERY_CHUNK_SIZE,
        })
    } else {
        Ok(())
    }
}

fn verify_identity_signature(
    validator_set: &ValidatorSet,
    validator_id: ValidatorId,
    signature: [u8; 64],
    message: &[u8],
) -> bool {
    let Some(credential) = validator_set.credential(validator_id) else {
        return false;
    };
    let Ok(key) = VerifyingKey::from_bytes(&credential.identity_public_key()) else {
        return false;
    };
    key.verify_strict(message, &Signature::from_bytes(&signature))
        .is_ok()
}

fn manifest_auth_bytes(binding: [u8; 32], validator_id: ValidatorId) -> Vec<u8> {
    let mut out = Vec::with_capacity(STATE_RECOVERY_AUTH_DOMAIN.len() + 4 + 32 + 8 + 1);
    out.extend_from_slice(STATE_RECOVERY_AUTH_DOMAIN);
    out.extend_from_slice(&CURRENT_NETWORK_PROTOCOL_VERSION.to_be_bytes());
    out.extend_from_slice(&binding);
    out.extend_from_slice(&validator_id.value().to_be_bytes());
    out.push(1);
    out
}

fn chunk_auth_bytes(
    binding: [u8; 32],
    validator_id: ValidatorId,
    checkpoint_digest: [u8; 32],
    offset: u64,
    limit: u32,
) -> Vec<u8> {
    let mut out =
        Vec::with_capacity(STATE_RECOVERY_AUTH_DOMAIN.len() + 4 + 32 + 8 + 1 + 32 + 8 + 4);
    out.extend_from_slice(STATE_RECOVERY_AUTH_DOMAIN);
    out.extend_from_slice(&CURRENT_NETWORK_PROTOCOL_VERSION.to_be_bytes());
    out.extend_from_slice(&binding);
    out.extend_from_slice(&validator_id.value().to_be_bytes());
    out.push(2);
    out.extend_from_slice(&checkpoint_digest);
    out.extend_from_slice(&offset.to_be_bytes());
    out.extend_from_slice(&limit.to_be_bytes());
    out
}
