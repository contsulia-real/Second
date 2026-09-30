use crate::support;

use second::{
    CURRENT_PROTOCOL_VERSION, ValidatorAdmissionError, ValidatorAdmissionRequest, ValidatorId,
};
use support::{key, validator_credential as credential};

#[test]
fn candidate_must_prove_possession_of_all_three_private_keys() {
    let candidate = credential(10);

    let request = ValidatorAdmissionRequest::sign(
        CURRENT_PROTOCOL_VERSION,
        candidate.clone(),
        &key(30),
        &key(31),
        &key(32),
    )
    .unwrap();

    let verified = request.verify().unwrap();
    assert_eq!(verified.credential(), &candidate);
}

#[test]
fn wrong_identity_key_is_rejected() {
    let candidate = credential(10);

    let request = ValidatorAdmissionRequest::sign(
        CURRENT_PROTOCOL_VERSION,
        candidate,
        &key(99),
        &key(31),
        &key(32),
    )
    .unwrap();

    assert_eq!(
        request.verify(),
        Err(ValidatorAdmissionError::InvalidIdentityProof(
            ValidatorId::new(10)
        ))
    );
}

#[test]
fn wrong_consensus_key_is_rejected() {
    let candidate = credential(10);

    let request = ValidatorAdmissionRequest::sign(
        CURRENT_PROTOCOL_VERSION,
        candidate,
        &key(30),
        &key(99),
        &key(32),
    )
    .unwrap();

    assert_eq!(
        request.verify(),
        Err(ValidatorAdmissionError::InvalidConsensusProof(
            ValidatorId::new(10)
        ))
    );
}

#[test]
fn wrong_recovery_key_is_rejected() {
    let candidate = credential(10);

    let request = ValidatorAdmissionRequest::sign(
        CURRENT_PROTOCOL_VERSION,
        candidate,
        &key(30),
        &key(31),
        &key(99),
    )
    .unwrap();

    assert_eq!(
        request.verify(),
        Err(ValidatorAdmissionError::InvalidRecoveryProof(
            ValidatorId::new(10)
        ))
    );
}

#[test]
fn protocol_version_is_bound_into_candidate_proof() {
    let candidate = credential(10);

    let request = ValidatorAdmissionRequest::sign(
        CURRENT_PROTOCOL_VERSION + 1,
        candidate,
        &key(30),
        &key(31),
        &key(32),
    )
    .unwrap();

    assert_eq!(
        request.verify(),
        Err(ValidatorAdmissionError::UnsupportedProtocolVersion {
            expected: CURRENT_PROTOCOL_VERSION,
            actual: CURRENT_PROTOCOL_VERSION + 1,
        })
    );
}
