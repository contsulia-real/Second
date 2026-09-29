use ed25519_dalek::SigningKey;
use second::{ValidatorCredential, ValidatorId, ValidatorSet};

fn credential(id: u64) -> ValidatorCredential {
    let consensus = SigningKey::from_bytes(&[id as u8; 32]);
    let identity = SigningKey::from_bytes(&[(id as u8).wrapping_add(40); 32]);
    let recovery = SigningKey::from_bytes(&[(id as u8).wrapping_add(80); 32]);
    ValidatorCredential::new(
        ValidatorId::new(id),
        identity.verifying_key().to_bytes(),
        consensus.verifying_key().to_bytes(),
        recovery.verifying_key().to_bytes(),
    )
    .unwrap()
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
