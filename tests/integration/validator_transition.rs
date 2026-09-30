use crate::support;

use second::{
    CURRENT_PROTOCOL_VERSION, CertifiedValidatorSetTransition,
    ValidatorConsensusKeyRotationRequest, ValidatorCredential, ValidatorId, ValidatorRegistry,
    ValidatorRotationAuthority, ValidatorSet, ValidatorSetTransition, ValidatorTransitionError,
};
use support::{key, signed_vote, validator_credential as credential};

fn current_set() -> ValidatorSet {
    ValidatorSet::new(4, (1..=4).map(credential)).unwrap()
}

fn registry(current: &ValidatorSet) -> ValidatorRegistry {
    ValidatorRegistry::from_validator_set(current).unwrap()
}

fn admission(id: u64) -> second::VerifiedValidatorAdmission {
    second::ValidatorAdmissionRequest::sign(
        CURRENT_PROTOCOL_VERSION,
        credential(id),
        &key((id * 3) as u8),
        &key((id * 3 + 1) as u8),
        &key((id * 3 + 2) as u8),
    )
    .unwrap()
    .verify()
    .unwrap()
}

#[test]
fn validator_credential_requires_three_distinct_keys() {
    let shared = key(7).verifying_key().to_bytes();

    assert!(
        ValidatorCredential::new(
            ValidatorId::new(1),
            shared,
            shared,
            key(8).verifying_key().to_bytes(),
        )
        .is_err()
    );
}

#[test]
fn validator_set_rejects_key_reuse_across_different_validators() {
    let first = credential(1);
    let reused_identity = ValidatorCredential::new(
        ValidatorId::new(2),
        first.identity_public_key(),
        key(20).verifying_key().to_bytes(),
        key(21).verifying_key().to_bytes(),
    )
    .unwrap();

    assert!(ValidatorSet::new(1, [first, reused_identity]).is_err());
}

#[test]
fn current_quorum_can_certify_complete_next_validator_set() {
    let current = current_set();
    let next = ValidatorSet::new(5, (1..=5).map(credential)).unwrap();
    let transition = ValidatorSetTransition::new(
        CURRENT_PROTOCOL_VERSION,
        9,
        &current,
        &registry(&current),
        next,
        vec![admission(5)],
        Vec::new(),
    )
    .unwrap();
    let statement = transition.finality_statement();

    let votes = [1_u64, 2, 3]
        .into_iter()
        .map(|id| signed_vote(&statement, ValidatorId::new(id), &key((id * 3 + 1) as u8)))
        .collect();

    let certified = CertifiedValidatorSetTransition::new(transition, votes, &current).unwrap();

    assert_eq!(certified.activation_epoch(), 10);
    assert_eq!(certified.next_validator_set().version(), 5);
    assert_eq!(certified.next_validator_set().len(), 5);
}

#[test]
fn joining_validator_cannot_contribute_a_vote_before_activation() {
    let current = current_set();
    let next = ValidatorSet::new(5, (1..=5).map(credential)).unwrap();
    let transition = ValidatorSetTransition::new(
        CURRENT_PROTOCOL_VERSION,
        9,
        &current,
        &registry(&current),
        next,
        vec![admission(5)],
        Vec::new(),
    )
    .unwrap();
    let statement = transition.finality_statement();

    let votes = vec![
        signed_vote(&statement, ValidatorId::new(1), &key(4)),
        signed_vote(&statement, ValidatorId::new(2), &key(7)),
        signed_vote(&statement, ValidatorId::new(5), &key(16)),
    ];

    assert_eq!(
        CertifiedValidatorSetTransition::new(transition, votes, &current),
        Err(ValidatorTransitionError::Finality(
            second::FinalityError::UnknownValidator(ValidatorId::new(5))
        ))
    );
}

#[test]
fn retained_validator_identity_key_cannot_be_rewritten() {
    let current = current_set();

    let mut next_credentials = (1..=4).map(credential).collect::<Vec<_>>();
    next_credentials[0] = ValidatorCredential::new(
        ValidatorId::new(1),
        key(90).verifying_key().to_bytes(),
        key(4).verifying_key().to_bytes(),
        key(5).verifying_key().to_bytes(),
    )
    .unwrap();

    let next = ValidatorSet::new(5, next_credentials).unwrap();

    assert_eq!(
        ValidatorSetTransition::new(
            CURRENT_PROTOCOL_VERSION,
            9,
            &current,
            &registry(&current),
            next,
            Vec::new(),
            Vec::new(),
        ),
        Err(ValidatorTransitionError::IdentityKeyChanged(
            ValidatorId::new(1)
        ))
    );
}

#[test]
fn retained_validator_recovery_key_cannot_be_rewritten() {
    let current = current_set();

    let mut next_credentials = (1..=4).map(credential).collect::<Vec<_>>();
    next_credentials[0] = ValidatorCredential::new(
        ValidatorId::new(1),
        key(3).verifying_key().to_bytes(),
        key(4).verifying_key().to_bytes(),
        key(90).verifying_key().to_bytes(),
    )
    .unwrap();
    let next = ValidatorSet::new(5, next_credentials).unwrap();

    assert_eq!(
        ValidatorSetTransition::new(
            CURRENT_PROTOCOL_VERSION,
            9,
            &current,
            &registry(&current),
            next,
            Vec::new(),
            Vec::new(),
        ),
        Err(ValidatorTransitionError::RecoveryKeyChanged(
            ValidatorId::new(1)
        ))
    );
}

#[test]
fn retained_validator_consensus_key_requires_rotation_request() {
    let current = current_set();

    let mut next_credentials = (1..=4).map(credential).collect::<Vec<_>>();
    next_credentials[0] = ValidatorCredential::new(
        ValidatorId::new(1),
        key(3).verifying_key().to_bytes(),
        key(90).verifying_key().to_bytes(),
        key(5).verifying_key().to_bytes(),
    )
    .unwrap();
    let next = ValidatorSet::new(5, next_credentials).unwrap();

    assert_eq!(
        ValidatorSetTransition::new(
            CURRENT_PROTOCOL_VERSION,
            9,
            &current,
            &registry(&current),
            next,
            Vec::new(),
            Vec::new(),
        ),
        Err(ValidatorTransitionError::MissingConsensusKeyRotation(
            ValidatorId::new(1)
        ))
    );
}

#[test]
fn identity_authorized_consensus_key_rotation_is_accepted() {
    let current = current_set();

    let mut next_credentials = (1..=4).map(credential).collect::<Vec<_>>();
    next_credentials[0] = ValidatorCredential::new(
        ValidatorId::new(1),
        key(3).verifying_key().to_bytes(),
        key(90).verifying_key().to_bytes(),
        key(5).verifying_key().to_bytes(),
    )
    .unwrap();
    let next = ValidatorSet::new(5, next_credentials).unwrap();

    let rotation = ValidatorConsensusKeyRotationRequest::sign(
        CURRENT_PROTOCOL_VERSION,
        ValidatorRotationAuthority::Identity,
        ValidatorId::new(1),
        4,
        10,
        key(90).verifying_key().to_bytes(),
        &key(3),
    )
    .unwrap();

    let transition = ValidatorSetTransition::new(
        CURRENT_PROTOCOL_VERSION,
        9,
        &current,
        &registry(&current),
        next,
        Vec::new(),
        vec![rotation],
    )
    .unwrap();

    assert_eq!(
        transition
            .next_validator_set()
            .validator(ValidatorId::new(1))
            .unwrap()
            .consensus_public_key(),
        key(90).verifying_key().to_bytes()
    );
}

#[test]
fn consensus_key_rotation_is_bound_to_activation_epoch() {
    let current = current_set();

    let mut next_credentials = (1..=4).map(credential).collect::<Vec<_>>();
    next_credentials[0] = ValidatorCredential::new(
        ValidatorId::new(1),
        key(3).verifying_key().to_bytes(),
        key(90).verifying_key().to_bytes(),
        key(5).verifying_key().to_bytes(),
    )
    .unwrap();
    let next = ValidatorSet::new(5, next_credentials).unwrap();

    let rotation = ValidatorConsensusKeyRotationRequest::sign(
        CURRENT_PROTOCOL_VERSION,
        ValidatorRotationAuthority::Recovery,
        ValidatorId::new(1),
        4,
        11,
        key(90).verifying_key().to_bytes(),
        &key(5),
    )
    .unwrap();

    assert_eq!(
        ValidatorSetTransition::new(
            CURRENT_PROTOCOL_VERSION,
            9,
            &current,
            &registry(&current),
            next,
            Vec::new(),
            vec![rotation],
        ),
        Err(
            ValidatorTransitionError::ConsensusKeyRotationEpochMismatch {
                validator_id: ValidatorId::new(1),
                expected: 10,
                actual: 11,
            }
        )
    );
}

#[test]
fn newly_added_validator_without_admission_proof_is_rejected() {
    let current = current_set();
    let next = ValidatorSet::new(5, (1..=5).map(credential)).unwrap();

    assert_eq!(
        ValidatorSetTransition::new(
            CURRENT_PROTOCOL_VERSION,
            9,
            &current,
            &registry(&current),
            next,
            Vec::new(),
            Vec::new(),
        ),
        Err(ValidatorTransitionError::MissingAdmission(
            ValidatorId::new(5)
        ))
    );
}

#[test]
fn admission_for_validator_not_added_to_next_set_is_rejected() {
    let current = current_set();
    let next = ValidatorSet::new(5, (1..=4).map(credential)).unwrap();

    assert_eq!(
        ValidatorSetTransition::new(
            CURRENT_PROTOCOL_VERSION,
            9,
            &current,
            &registry(&current),
            next,
            vec![admission(6)],
            Vec::new(),
        ),
        Err(ValidatorTransitionError::UnexpectedAdmission(
            ValidatorId::new(6)
        ))
    );
}

#[test]
fn duplicate_admission_proof_is_rejected() {
    let current = current_set();
    let next = ValidatorSet::new(5, (1..=5).map(credential)).unwrap();
    let proof = admission(5);

    assert_eq!(
        ValidatorSetTransition::new(
            CURRENT_PROTOCOL_VERSION,
            9,
            &current,
            &registry(&current),
            next,
            vec![proof.clone(), proof],
            Vec::new(),
        ),
        Err(ValidatorTransitionError::DuplicateAdmission(
            ValidatorId::new(5)
        ))
    );
}

#[test]
fn certified_transition_only_activates_at_its_declared_epoch() {
    let current = current_set();
    let next = ValidatorSet::new(5, (1..=5).map(credential)).unwrap();
    let transition = ValidatorSetTransition::new(
        CURRENT_PROTOCOL_VERSION,
        9,
        &current,
        &registry(&current),
        next,
        vec![admission(5)],
        Vec::new(),
    )
    .unwrap();
    let statement = transition.finality_statement();
    let votes = [1_u64, 2, 3]
        .into_iter()
        .map(|id| signed_vote(&statement, ValidatorId::new(id), &key((id * 3 + 1) as u8)))
        .collect();
    let certified = CertifiedValidatorSetTransition::new(transition, votes, &current).unwrap();
    let mut registry = registry(&current);

    assert_eq!(
        certified.clone().activate(9, &mut registry),
        Err(ValidatorTransitionError::WrongActivationEpoch {
            expected: 10,
            actual: 9,
        })
    );

    let activated = certified.activate(10, &mut registry).unwrap();
    assert_eq!(activated.version(), 5);
    assert_eq!(activated.len(), 5);
}
