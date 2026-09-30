mod support;

use second::{ValidatorCredential, ValidatorId, ValidatorSet};
use support::key;

fn credential(id: u64) -> ValidatorCredential {
    let consensus = key(id as u8);
    let identity = key((id as u8).wrapping_add(40));
    let recovery = key((id as u8).wrapping_add(80));
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
