mod support;

use second::{
    CURRENT_PROTOCOL_VERSION, ValidatorConsensusKeyRotationRequest, ValidatorId,
    ValidatorRotationAuthority, ValidatorRotationError,
};
use support::{key, validator_credential};

#[test]
fn identity_key_can_authorize_consensus_key_rotation() {
    let current = validator_credential(7);
    let request = ValidatorConsensusKeyRotationRequest::sign(
        CURRENT_PROTOCOL_VERSION,
        ValidatorRotationAuthority::Identity,
        ValidatorId::new(7),
        4,
        10,
        key(50).verifying_key().to_bytes(),
        &key(21),
    )
    .unwrap();

    request.verify(&current).unwrap();
}

#[test]
fn recovery_key_can_authorize_consensus_key_rotation() {
    let current = validator_credential(7);
    let request = ValidatorConsensusKeyRotationRequest::sign(
        CURRENT_PROTOCOL_VERSION,
        ValidatorRotationAuthority::Recovery,
        ValidatorId::new(7),
        4,
        10,
        key(50).verifying_key().to_bytes(),
        &key(23),
    )
    .unwrap();

    request.verify(&current).unwrap();
}

#[test]
fn consensus_key_cannot_authorize_its_own_rotation() {
    let current = validator_credential(7);
    let request = ValidatorConsensusKeyRotationRequest::sign(
        CURRENT_PROTOCOL_VERSION,
        ValidatorRotationAuthority::Identity,
        ValidatorId::new(7),
        4,
        10,
        key(50).verifying_key().to_bytes(),
        &key(22),
    )
    .unwrap();

    assert_eq!(
        request.verify(&current),
        Err(ValidatorRotationError::InvalidAuthorization(
            ValidatorId::new(7)
        ))
    );
}

#[test]
fn rotation_request_is_bound_to_validator_set_and_activation_epoch() {
    let current = validator_credential(7);
    let request = ValidatorConsensusKeyRotationRequest::sign(
        CURRENT_PROTOCOL_VERSION,
        ValidatorRotationAuthority::Identity,
        ValidatorId::new(7),
        4,
        10,
        key(50).verifying_key().to_bytes(),
        &key(21),
    )
    .unwrap();

    assert_eq!(request.current_validator_set_version(), 4);
    assert_eq!(request.activation_epoch(), 10);
    assert_eq!(
        request.new_consensus_public_key(),
        key(50).verifying_key().to_bytes()
    );
    request.verify(&current).unwrap();
}
