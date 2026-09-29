use ed25519_dalek::SigningKey;
use second::{ValidatorCredential, ValidatorId, ValidatorSet};

fn credential(id: u64) -> ValidatorCredential {
    let key = SigningKey::from_bytes(&[id as u8; 32]);
    ValidatorCredential::new(ValidatorId::new(id), key.verifying_key().to_bytes()).unwrap()
}

#[test]
fn validators_are_equal_weight_and_quorum_is_two_thirds_plus_one() {
    let set = ValidatorSet::new(7, (1..=7).map(credential)).unwrap();

    assert_eq!(set.len(), 7);
    assert_eq!(set.quorum_threshold(), 5);
    assert!(!set.has_quorum([
        ValidatorId::new(1),
        ValidatorId::new(2),
        ValidatorId::new(3),
        ValidatorId::new(4),
    ]));
    assert!(set.has_quorum([
        ValidatorId::new(1),
        ValidatorId::new(2),
        ValidatorId::new(3),
        ValidatorId::new(4),
        ValidatorId::new(5),
    ]));
}

#[test]
fn duplicate_votes_do_not_increase_vote_count() {
    let set = ValidatorSet::new(4, (1..=4).map(credential)).unwrap();

    assert!(!set.has_quorum([
        ValidatorId::new(1),
        ValidatorId::new(1),
        ValidatorId::new(2),
    ]));
}
