//! Fetch the installed immutable handoff using current-member identity rights.
use super::*;
use crate::persistence::TaskHandoff;
use crate::{PersistedNodeState, ValidatorSetTransitionProof};

const HANDOFF_AUTH_DOMAIN: &[u8] = b"SECOND_HANDOFF_BASELINE_AUTH_V1\0";
#[cfg(test)]
pub(crate) mod tests;

pub(super) fn handoff_chunk_response(
    peer: &QuicPeer,
    message: &NetworkMessage,
    snapshot: &PersistedNodeState,
) -> Option<Result<NetworkMessage, NetworkError>> {
    let NetworkMessage::GetStateRecoveryChunk {
        validator_id,
        checkpoint_digest,
        offset,
        limit,
        signature,
    } = message
    else {
        return None;
    };
    let handoff = snapshot.state.protocol.task_handoff.as_ref()?;
    let certifier = handoff.certifier_set.as_ref()?;
    let proof = snapshot
        .validator_transition_proofs
        .get(&certifier.version())?;
    if proof.source().next_validator_set() != &snapshot.validator_set
        || proof.source().task_handoff_digest() != Some(*checkpoint_digest)
    {
        return None;
    }
    Some((|| {
        let auth = StateRecoveryRequestContext {
            checkpoint_digest: Some(*checkpoint_digest),
            validator_set: &snapshot.validator_set,
            validator_id: *validator_id,
            signature: *signature,
        };
        if !authenticate_chunk(
            peer,
            &auth,
            *checkpoint_digest,
            *offset,
            *limit,
            HANDOFF_AUTH_DOMAIN,
        )? {
            return Ok(NetworkMessage::StateRecoveryDenied);
        }
        let body = handoff
            .encode_shared()
            .map_err(|_| NetworkError::InvalidStateRecoveryPayload)?;
        let prefix = (body.len() as u64).to_be_bytes();
        let start =
            usize::try_from(*offset).map_err(|_| NetworkError::InvalidStateRecoveryChunk)?;
        let total = prefix.len() + body.len();
        if start >= total {
            return Err(NetworkError::InvalidStateRecoveryChunk);
        }
        let end = start.saturating_add(*limit as usize).min(total);
        let mut bytes = Vec::with_capacity(end - start);
        if start < prefix.len() {
            bytes.extend_from_slice(&prefix[start..end.min(prefix.len())]);
        }
        if end > prefix.len() {
            bytes.extend_from_slice(&body[start.saturating_sub(prefix.len())..end - prefix.len()]);
        }
        Ok(NetworkMessage::StateRecoveryChunk {
            checkpoint_digest: *checkpoint_digest,
            offset: *offset,
            bytes,
        })
    })())
}

/// The trust anchor must come from local trusted committee/registry state.
/// This returns authenticated body bytes; installation still verifies authorizers
/// and uses the existing atomic, signing-locked handoff installer.
/// Use a fresh connection for body requests: a connection opened with a public
/// transition-proof query remains a proof-only session.
pub async fn client_fetch_validator_handoff(
    peer: &QuicPeer,
    validator_id: ValidatorId,
    identity_key: &SigningKey,
    proof: &ValidatorSetTransitionProof,
    trusted: &PersistedNodeState,
) -> Result<Vec<u8>, NetworkError> {
    proof
        .verify(&trusted.validator_set, &trusted.validator_registry)
        .map_err(|_| NetworkError::InvalidValidatorTransitionProof)?;
    if proof
        .source()
        .next_validator_set()
        .credential(validator_id)
        .is_none_or(|credential| {
            credential.identity_public_key() != identity_key.verifying_key().to_bytes()
        })
    {
        return Err(NetworkError::StateRecoveryUnauthorized);
    }
    let Some(digest) = proof.source().task_handoff_digest() else {
        if proof.source().currency_frontier() != 1 {
            return Err(NetworkError::InvalidValidatorTransitionProof);
        }
        return TaskHandoff::empty_for(&trusted.validator_set)
            .encode()
            .map_err(|_| NetworkError::InvalidStateRecoveryPayload);
    };
    let client = StateChunkClient {
        peer,
        validator_id,
        identity_key,
        digest,
        domain: HANDOFF_AUTH_DOMAIN,
    };
    let first = client.chunk(0, MAX_STATE_RECOVERY_CHUNK_SIZE).await?;
    let (prefix, bytes) = first
        .split_at_checked(8)
        .ok_or(NetworkError::InvalidStateRecoveryChunk)?;
    let length = u64::from_be_bytes(
        prefix
            .try_into()
            .map_err(|_| NetworkError::InvalidStateRecoveryChunk)?,
    );
    let length = usize::try_from(length).map_err(|_| NetworkError::InvalidStateRecoveryChunk)?;
    if first.len()
        != length
            .saturating_add(8)
            .min(MAX_STATE_RECOVERY_CHUNK_SIZE as usize)
    {
        return Err(NetworkError::InvalidStateRecoveryChunk);
    }
    let body = client.fetch_payload(length, bytes.to_vec(), 8).await?;
    let handoff =
        TaskHandoff::decode(&body).map_err(|_| NetworkError::InvalidStateRecoveryPayload)?;
    if handoff.certifier_set.as_ref() != Some(&trusted.validator_set)
        || handoff
            .digest()
            .map_err(|_| NetworkError::InvalidStateRecoveryPayload)?
            != digest
    {
        return Err(NetworkError::InvalidStateRecoveryPayload);
    }
    Ok(body)
}
