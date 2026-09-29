use ed25519_dalek::SigningKey;
use second::{
    CURRENT_PROTOCOL_VERSION, CertifiedValidatorSetTransition, ValidatorCredential, ValidatorId,
    ValidatorSet, ValidatorSetTransition, ValidatorTransitionError, ValidatorVote,
};

fn key(byte: u8) -> SigningKey {
    SigningKey::from_bytes(&[byte; 32])
}

fn credential(id: u64) -> ValidatorCredential {
    ValidatorCredential::new(
        ValidatorId::new(id),
        key((id * 3) as u8).verifying_key().to_bytes(),
        key((id * 3 + 1) as u8).verifying_key().to_bytes(),
        key((id * 3 + 2) as u8).verifying_key().to_bytes(),
    )
    .unwrap()
}

fn current_set() -> ValidatorSet {
    ValidatorSet::new(4, (1..=4).map(credential)).unwrap()
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
    let transition =
        ValidatorSetTransition::new(CURRENT_PROTOCOL_VERSION, 9, &current, next).unwrap();
    let statement = transition.finality_statement();

    let votes = [1_u64, 2, 3]
        .into_iter()
        .map(|id| ValidatorVote::sign(&statement, ValidatorId::new(id), &key((id * 3 + 1) as u8)))
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
    let transition =
        ValidatorSetTransition::new(CURRENT_PROTOCOL_VERSION, 9, &current, next).unwrap();
    let statement = transition.finality_statement();

    let votes = vec![
        ValidatorVote::sign(&statement, ValidatorId::new(1), &key(4)),
        ValidatorVote::sign(&statement, ValidatorId::new(2), &key(7)),
        ValidatorVote::sign(&statement, ValidatorId::new(5), &key(16)),
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
        ValidatorSetTransition::new(CURRENT_PROTOCOL_VERSION, 9, &current, next),
        Err(ValidatorTransitionError::IdentityKeyChanged(
            ValidatorId::new(1)
        ))
    );
}

#[test]
fn certified_transition_only_activates_at_its_declared_epoch() {
    let current = current_set();
    let next = ValidatorSet::new(5, (1..=5).map(credential)).unwrap();
    let transition =
        ValidatorSetTransition::new(CURRENT_PROTOCOL_VERSION, 9, &current, next).unwrap();
    let statement = transition.finality_statement();
    let votes = [1_u64, 2, 3]
        .into_iter()
        .map(|id| ValidatorVote::sign(&statement, ValidatorId::new(id), &key((id * 3 + 1) as u8)))
        .collect();
    let certified = CertifiedValidatorSetTransition::new(transition, votes, &current).unwrap();

    assert_eq!(
        certified.clone().activate(9),
        Err(ValidatorTransitionError::WrongActivationEpoch {
            expected: 10,
            actual: 9,
        })
    );

    let activated = certified.activate(10).unwrap();
    assert_eq!(activated.version(), 5);
    assert_eq!(activated.len(), 5);
}
