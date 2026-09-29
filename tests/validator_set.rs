use second::{ValidatorId, ValidatorSet};

#[test]
fn validators_are_equal_weight_and_quorum_is_two_thirds_plus_one() {
    let set = ValidatorSet::new(7, (1..=7).map(ValidatorId::new)).unwrap();

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
    let set = ValidatorSet::new(4, (1..=4).map(ValidatorId::new)).unwrap();

    assert!(!set.has_quorum([
        ValidatorId::new(1),
        ValidatorId::new(1),
        ValidatorId::new(2),
    ]));
}
