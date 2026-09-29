use ed25519_dalek::SigningKey;
use second::{
    CURRENT_PROTOCOL_VERSION, CertifiedPublicCurrencyCheckpoint, FinalityError,
    PublicCurrencyCheckpoint, PublicCurrencyCheckpointProof, SecondState, ValidatorCredential,
    ValidatorId, ValidatorSet, ValidatorVote,
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
fn unverified_checkpoint_proof_must_be_revalidated_against_view_and_validator_set() {
    let validators = validators();
    let state = SecondState::genesis([], 1).with_reserve(2).unwrap();
    let summary = state.public_currency_summary();
    let view =
        second::PublicCurrencyView::new(summary.clone(), state.public_currency_states()).unwrap();
    let checkpoint = PublicCurrencyCheckpoint::new(CURRENT_PROTOCOL_VERSION, 10, summary);
    let proof = PublicCurrencyCheckpointProof::new(
        checkpoint.clone(),
        validators.version(),
        votes(&checkpoint, &validators, &[1, 2, 3]),
    );

    let certified = proof.verify(&view, &validators).unwrap();

    assert_eq!(certified.checkpoint(), &checkpoint);
    assert_eq!(certified.certificate().vote_count(), 3);
}

#[test]
fn certified_checkpoint_accepts_only_the_matching_verified_public_view() {
    let validators = validators();
    let state = SecondState::genesis([], 1).with_reserve(2).unwrap();
    let summary = state.public_currency_summary();
    let view =
        second::PublicCurrencyView::new(summary.clone(), state.public_currency_states()).unwrap();
    let checkpoint = PublicCurrencyCheckpoint::new(CURRENT_PROTOCOL_VERSION, 10, summary);

    let certified = CertifiedPublicCurrencyCheckpoint::new(
        checkpoint.clone(),
        votes(&checkpoint, &validators, &[1, 2, 3]),
        &validators,
    )
    .unwrap();

    assert_eq!(certified.verify_view(&view, &validators), Ok(()));
}

#[test]
fn certified_checkpoint_rejects_a_different_public_view() {
    let validators = validators();
    let state = SecondState::genesis([], 1).with_reserve(2).unwrap();
    let other_state = SecondState::genesis([], 10).with_reserve(2).unwrap();

    let summary = state.public_currency_summary();
    let checkpoint = PublicCurrencyCheckpoint::new(CURRENT_PROTOCOL_VERSION, 10, summary);
    let certified = CertifiedPublicCurrencyCheckpoint::new(
        checkpoint.clone(),
        votes(&checkpoint, &validators, &[1, 2, 3]),
        &validators,
    )
    .unwrap();

    let other_view = second::PublicCurrencyView::new(
        other_state.public_currency_summary(),
        other_state.public_currency_states(),
    )
    .unwrap();

    assert_eq!(
        certified.verify_view(&other_view, &validators),
        Err(second::PublicCheckpointError::SummaryMismatch)
    );
}

#[test]
fn certified_checkpoint_is_bound_to_validator_set_version() {
    let validators = validators();
    let other_version = ValidatorSet::new(5, (1..=4).map(credential)).unwrap();
    let state = SecondState::genesis([], 1).with_reserve(2).unwrap();
    let summary = state.public_currency_summary();
    let view =
        second::PublicCurrencyView::new(summary.clone(), state.public_currency_states()).unwrap();
    let checkpoint = PublicCurrencyCheckpoint::new(CURRENT_PROTOCOL_VERSION, 10, summary);

    let certified = CertifiedPublicCurrencyCheckpoint::new(
        checkpoint.clone(),
        votes(&checkpoint, &validators, &[1, 2, 3]),
        &validators,
    )
    .unwrap();

    assert_eq!(
        certified.verify_view(&view, &other_version),
        Err(second::PublicCheckpointError::Finality(
            FinalityError::WrongValidatorSetVersion {
                expected: 5,
                actual: 4,
            }
        ))
    );
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
