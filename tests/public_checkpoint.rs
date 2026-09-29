use ed25519_dalek::SigningKey;
use second::{
    CURRENT_PROTOCOL_VERSION, CertifiedPublicCurrencyCheckpoint, FinalityError,
    PublicCurrencyCheckpoint, SecondState, ValidatorCredential, ValidatorId, ValidatorSet,
    ValidatorVote,
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

fn validators() -> ValidatorSet {
    ValidatorSet::new(4, (1..=4).map(credential)).unwrap()
}

fn votes(
    checkpoint: &PublicCurrencyCheckpoint,
    validators: &ValidatorSet,
    ids: &[u64],
) -> Vec<ValidatorVote> {
    let statement = checkpoint.finality_statement(validators.version());
    ids.iter()
        .map(|id| ValidatorVote::sign(&statement, ValidatorId::new(*id), &key((*id * 3 + 1) as u8)))
        .collect()
}

#[test]
fn checkpoint_digest_commits_epoch_and_exact_public_summary() {
    let reserve = SecondState::genesis([], 1).with_reserve(2).unwrap();
    let other_reserve = SecondState::genesis([], 10).with_reserve(2).unwrap();

    let epoch_10 = PublicCurrencyCheckpoint::new(
        CURRENT_PROTOCOL_VERSION,
        10,
        reserve.public_currency_summary(),
    );
    let epoch_11 = PublicCurrencyCheckpoint::new(
        CURRENT_PROTOCOL_VERSION,
        11,
        reserve.public_currency_summary(),
    );
    let different_summary = PublicCurrencyCheckpoint::new(
        CURRENT_PROTOCOL_VERSION,
        10,
        other_reserve.public_currency_summary(),
    );

    assert_ne!(epoch_10.digest(), epoch_11.digest());
    assert_ne!(epoch_10.digest(), different_summary.digest());
}

#[test]
fn three_of_four_validators_certify_the_exact_checkpoint() {
    let validators = validators();
    let state = SecondState::genesis([], 1).with_reserve(2).unwrap();
    let checkpoint = PublicCurrencyCheckpoint::new(
        CURRENT_PROTOCOL_VERSION,
        10,
        state.public_currency_summary(),
    );

    let certified = CertifiedPublicCurrencyCheckpoint::new(
        checkpoint.clone(),
        votes(&checkpoint, &validators, &[1, 2, 3]),
        &validators,
    )
    .unwrap();

    assert_eq!(certified.checkpoint(), &checkpoint);
    assert_eq!(certified.certificate().vote_count(), 3);
}

#[test]
fn two_of_four_validators_cannot_certify_public_checkpoint() {
    let validators = validators();
    let state = SecondState::genesis([], 1).with_reserve(2).unwrap();
    let checkpoint = PublicCurrencyCheckpoint::new(
        CURRENT_PROTOCOL_VERSION,
        10,
        state.public_currency_summary(),
    );

    assert_eq!(
        CertifiedPublicCurrencyCheckpoint::new(
            checkpoint.clone(),
            votes(&checkpoint, &validators, &[1, 2]),
            &validators,
        ),
        Err(FinalityError::InsufficientVotes {
            required: 3,
            actual: 2,
        })
    );
}

#[test]
fn votes_for_one_checkpoint_cannot_certify_a_different_epoch() {
    let validators = validators();
    let state = SecondState::genesis([], 1).with_reserve(2).unwrap();
    let checkpoint_10 = PublicCurrencyCheckpoint::new(
        CURRENT_PROTOCOL_VERSION,
        10,
        state.public_currency_summary(),
    );
    let checkpoint_11 = PublicCurrencyCheckpoint::new(
        CURRENT_PROTOCOL_VERSION,
        11,
        state.public_currency_summary(),
    );
    let epoch_10_votes = votes(&checkpoint_10, &validators, &[1, 2, 3]);

    assert_eq!(
        CertifiedPublicCurrencyCheckpoint::new(checkpoint_11, epoch_10_votes, &validators,),
        Err(FinalityError::InvalidSignature(ValidatorId::new(1)))
    );
}
