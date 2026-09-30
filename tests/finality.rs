mod support;

use second::{
    CURRENT_PROTOCOL_VERSION, FinalityCertificate, FinalityError, FinalityStatement,
    ValidatorCredential, ValidatorId, ValidatorSet,
};
use support::{key, signed_vote};

fn validator(id: u64, key_byte: u8) -> ValidatorCredential {
    ValidatorCredential::new(
        ValidatorId::new(id),
        key(key_byte.wrapping_add(40)).verifying_key().to_bytes(),
        key(key_byte).verifying_key().to_bytes(),
        key(key_byte.wrapping_add(80)).verifying_key().to_bytes(),
    )
    .unwrap()
}

fn set4() -> ValidatorSet {
    ValidatorSet::new(
        11,
        [
            validator(1, 1),
            validator(2, 2),
            validator(3, 3),
            validator(4, 4),
        ],
    )
    .unwrap()
}

fn statement() -> FinalityStatement {
    FinalityStatement::new(CURRENT_PROTOCOL_VERSION, 11, [9; 32])
}

#[test]
fn three_of_four_equal_weight_validators_finalize_a_statement() {
    let set = set4();
    let statement = statement();
    let votes = vec![
        signed_vote(&statement, ValidatorId::new(1), &key(1)),
        signed_vote(&statement, ValidatorId::new(2), &key(2)),
        signed_vote(&statement, ValidatorId::new(3), &key(3)),
    ];

    let certificate = FinalityCertificate::new(statement, votes, &set).unwrap();

    assert_eq!(certificate.vote_count(), 3);
    certificate.verify(&set).unwrap();
}

#[test]
fn two_of_four_votes_are_not_enough() {
    let set = set4();
    let statement = statement();
    let votes = vec![
        signed_vote(&statement, ValidatorId::new(1), &key(1)),
        signed_vote(&statement, ValidatorId::new(2), &key(2)),
    ];

    assert_eq!(
        FinalityCertificate::new(statement, votes, &set),
        Err(FinalityError::InsufficientVotes {
            required: 3,
            actual: 2,
        })
    );
}

#[test]
fn duplicate_validator_votes_are_rejected_not_double_counted() {
    let set = set4();
    let statement = statement();
    let vote = signed_vote(&statement, ValidatorId::new(1), &key(1));

    assert_eq!(
        FinalityCertificate::new(
            statement,
            vec![
                vote.clone(),
                vote,
                signed_vote(&statement, ValidatorId::new(2), &key(2)),
            ],
            &set,
        ),
        Err(FinalityError::DuplicateVote(ValidatorId::new(1)))
    );
}

#[test]
fn signature_from_the_wrong_consensus_key_is_rejected() {
    let set = set4();
    let statement = statement();
    let votes = vec![
        signed_vote(&statement, ValidatorId::new(1), &key(99)),
        signed_vote(&statement, ValidatorId::new(2), &key(2)),
        signed_vote(&statement, ValidatorId::new(3), &key(3)),
    ];

    assert_eq!(
        FinalityCertificate::new(statement, votes, &set),
        Err(FinalityError::InvalidSignature(ValidatorId::new(1)))
    );
}

#[test]
fn finality_is_bound_to_the_exact_validator_set_version() {
    let set = set4();
    let statement = FinalityStatement::new(CURRENT_PROTOCOL_VERSION, 12, [9; 32]);

    assert_eq!(
        FinalityCertificate::new(statement, Vec::new(), &set),
        Err(FinalityError::WrongValidatorSetVersion {
            expected: 11,
            actual: 12,
        })
    );
}

#[test]
fn duplicate_consensus_keys_are_not_valid_validator_credentials_in_one_set() {
    let shared = key(7).verifying_key().to_bytes();

    assert!(
        ValidatorSet::new(
            1,
            [
                ValidatorCredential::new(
                    ValidatorId::new(1),
                    key(41).verifying_key().to_bytes(),
                    shared,
                    key(81).verifying_key().to_bytes(),
                )
                .unwrap(),
                ValidatorCredential::new(
                    ValidatorId::new(2),
                    key(42).verifying_key().to_bytes(),
                    shared,
                    key(82).verifying_key().to_bytes(),
                )
                .unwrap(),
            ],
        )
        .is_err()
    );
}
