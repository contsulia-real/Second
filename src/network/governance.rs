use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};

use crate::{
    MAX_VALIDATOR_TRANSITION_SOURCE_SIZE, StateRecoveryCheckpoint, ValidatorId, ValidatorSet,
    ValidatorSetTransitionSource,
};

use super::{
    CURRENT_NETWORK_PROTOCOL_VERSION, GovernanceRejection, NetworkError, NetworkMessage, QuicPeer,
};

const GOVERNANCE_AUTH_DOMAIN: &[u8] = b"SECOND_GOVERNANCE_AUTH_V1\0";
const TRANSITION_ACTION: u8 = 1;
const RECOVERY_ACTION: u8 = 2;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteValidatorTransitionSubmission {
    pub current_validator_set_version: u64,
    pub next_validator_set_version: u64,
    pub transition_digest: [u8; 32],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteRecoveryCheckpointSubmission {
    pub validator_set_version: u64,
    pub serial: u64,
    pub checkpoint_digest: [u8; 32],
}

pub async fn client_submit_validator_transition(
    peer: &QuicPeer,
    validator_id: ValidatorId,
    identity_key: &SigningKey,
    validator_set_version: u64,
    source: &ValidatorSetTransitionSource,
) -> Result<RemoteValidatorTransitionSubmission, NetworkError> {
    let source = source
        .encode_bytes()
        .map_err(|_| NetworkError::InvalidGovernanceRequest)?;
    if source.is_empty() || source.len() > MAX_VALIDATOR_TRANSITION_SOURCE_SIZE {
        return Err(NetworkError::InvalidGovernanceRequest);
    }
    let payload_digest = Sha256::digest(&source).into();
    let signature = sign_governance_request(
        peer,
        identity_key,
        TRANSITION_ACTION,
        validator_id,
        validator_set_version,
        payload_digest,
    )?;
    match peer
        .exchange(&NetworkMessage::ValidatorTransitionSubmit {
            validator_id,
            validator_set_version,
            source,
            signature,
        })
        .await?
    {
        NetworkMessage::ValidatorTransitionAccepted {
            current_validator_set_version,
            next_validator_set_version,
            transition_digest,
        } => Ok(RemoteValidatorTransitionSubmission {
            current_validator_set_version,
            next_validator_set_version,
            transition_digest,
        }),
        NetworkMessage::GovernanceRejected { reason } => {
            Err(NetworkError::GovernanceRejected(reason))
        }
        _ => Err(NetworkError::UnexpectedMessage),
    }
}

pub async fn client_submit_recovery_checkpoint(
    peer: &QuicPeer,
    validator_id: ValidatorId,
    identity_key: &SigningKey,
    validator_set_version: u64,
) -> Result<RemoteRecoveryCheckpointSubmission, NetworkError> {
    let signature = sign_governance_request(
        peer,
        identity_key,
        RECOVERY_ACTION,
        validator_id,
        validator_set_version,
        [0_u8; 32],
    )?;
    match peer
        .exchange(&NetworkMessage::StateRecoveryCheckpointSubmit {
            validator_id,
            validator_set_version,
            signature,
        })
        .await?
    {
        NetworkMessage::StateRecoveryCheckpointAccepted {
            validator_set_version,
            serial,
            checkpoint_digest,
        } => Ok(RemoteRecoveryCheckpointSubmission {
            validator_set_version,
            serial,
            checkpoint_digest,
        }),
        NetworkMessage::GovernanceRejected { reason } => {
            Err(NetworkError::GovernanceRejected(reason))
        }
        _ => Err(NetworkError::UnexpectedMessage),
    }
}

pub(crate) fn verify_transition_request(
    peer: &QuicPeer,
    validator_id: ValidatorId,
    validator_set_version: u64,
    source: &[u8],
    signature: [u8; 64],
    validator_set: &ValidatorSet,
) -> Result<(), NetworkError> {
    if source.is_empty()
        || source.len() > MAX_VALIDATOR_TRANSITION_SOURCE_SIZE
        || validator_set_version != validator_set.version()
    {
        return Err(NetworkError::GovernanceUnauthorized);
    }
    let payload_digest = Sha256::digest(source).into();
    verify_governance_request(
        peer,
        TRANSITION_ACTION,
        validator_id,
        validator_set_version,
        payload_digest,
        signature,
        validator_set,
    )
}

pub(crate) fn verify_recovery_request(
    peer: &QuicPeer,
    validator_id: ValidatorId,
    validator_set_version: u64,
    signature: [u8; 64],
    validator_set: &ValidatorSet,
) -> Result<(), NetworkError> {
    if validator_set_version != validator_set.version() {
        return Err(NetworkError::GovernanceUnauthorized);
    }
    verify_governance_request(
        peer,
        RECOVERY_ACTION,
        validator_id,
        validator_set_version,
        [0_u8; 32],
        signature,
        validator_set,
    )
}

pub(crate) fn transition_accepted(transition: &crate::ValidatorSetTransition) -> NetworkMessage {
    NetworkMessage::ValidatorTransitionAccepted {
        current_validator_set_version: transition.current_validator_set_version(),
        next_validator_set_version: transition.next_validator_set().version(),
        transition_digest: transition.digest(),
    }
}

pub(crate) fn recovery_accepted(checkpoint: &StateRecoveryCheckpoint) -> NetworkMessage {
    NetworkMessage::StateRecoveryCheckpointAccepted {
        validator_set_version: checkpoint.validator_set_version(),
        serial: checkpoint.serial(),
        checkpoint_digest: checkpoint.digest(),
    }
}

pub(crate) const fn rejected(reason: GovernanceRejection) -> NetworkMessage {
    NetworkMessage::GovernanceRejected { reason }
}

fn sign_governance_request(
    peer: &QuicPeer,
    identity_key: &SigningKey,
    action: u8,
    validator_id: ValidatorId,
    validator_set_version: u64,
    payload_digest: [u8; 32],
) -> Result<[u8; 64], NetworkError> {
    let binding = peer.channel_binding()?;
    Ok(identity_key
        .sign(&governance_auth_bytes(
            binding,
            action,
            validator_id,
            validator_set_version,
            payload_digest,
        ))
        .to_bytes())
}

fn verify_governance_request(
    peer: &QuicPeer,
    action: u8,
    validator_id: ValidatorId,
    validator_set_version: u64,
    payload_digest: [u8; 32],
    signature: [u8; 64],
    validator_set: &ValidatorSet,
) -> Result<(), NetworkError> {
    let credential = validator_set
        .validator(validator_id)
        .ok_or(NetworkError::GovernanceUnauthorized)?;
    let verifying_key = VerifyingKey::from_bytes(&credential.identity_public_key())
        .map_err(|_| NetworkError::GovernanceUnauthorized)?;
    let binding = peer.channel_binding()?;
    verifying_key
        .verify_strict(
            &governance_auth_bytes(
                binding,
                action,
                validator_id,
                validator_set_version,
                payload_digest,
            ),
            &Signature::from_bytes(&signature),
        )
        .map_err(|_| NetworkError::GovernanceUnauthorized)
}

fn governance_auth_bytes(
    binding: [u8; 32],
    action: u8,
    validator_id: ValidatorId,
    validator_set_version: u64,
    payload_digest: [u8; 32],
) -> Vec<u8> {
    let mut bytes =
        Vec::with_capacity(GOVERNANCE_AUTH_DOMAIN.len() + 4 + 1 + 8 + 8 + 32 + binding.len());
    bytes.extend_from_slice(GOVERNANCE_AUTH_DOMAIN);
    bytes.extend_from_slice(&CURRENT_NETWORK_PROTOCOL_VERSION.to_be_bytes());
    bytes.push(action);
    bytes.extend_from_slice(&validator_id.value().to_be_bytes());
    bytes.extend_from_slice(&validator_set_version.to_be_bytes());
    bytes.extend_from_slice(&payload_digest);
    bytes.extend_from_slice(&binding);
    bytes
}
