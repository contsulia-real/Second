use std::fs;

use crate::support;
use support::FinalizedExecute as _;

use second::{
    CURRENT_PROTOCOL_VERSION, Operation, PersistedNodeState, PublicCurrencyCheckpoint,
    PublicCurrencyCheckpointProof, SecondState, StateStore, ValidatorCredential, ValidatorId,
    ValidatorSet, ValidatorVote,
};
use support::{
    key, payment_address, register_payment_addresses, signed_vote, temp_base, verified_task,
};

fn validators() -> ValidatorSet {
    ValidatorSet::new(
        7,
        (1..=4).map(|id| {
            ValidatorCredential::new(
                ValidatorId::new(id),
                key((id as u8).wrapping_add(40)).verifying_key().to_bytes(),
                key(id as u8).verifying_key().to_bytes(),
                key((id as u8).wrapping_add(80)).verifying_key().to_bytes(),
            )
            .unwrap()
        }),
    )
    .unwrap()
}

fn checkpoint_proof(
    state: &SecondState,
    validators: &ValidatorSet,
    epoch: u64,
) -> PublicCurrencyCheckpointProof {
    let checkpoint = PublicCurrencyCheckpoint::new(
        CURRENT_PROTOCOL_VERSION,
        epoch,
        state.public_currency_summary(),
    );
    let statement = checkpoint.finality_statement(validators.version());
    let votes = [1_u64, 2, 3]
        .into_iter()
        .map(|id| signed_vote(&statement, ValidatorId::new(id), &key(id as u8)))
        .collect();

    PublicCurrencyCheckpointProof::new(checkpoint, validators.version(), votes)
}

#[test]
fn snapshot_restores_unverified_public_checkpoint_proof_without_auto_certifying_it() {
    let base = temp_base("checkpoint-proof");
    let store = StateStore::new(&base);
    let state = SecondState::genesis([], 10).with_reserve(3).unwrap();
    let set = validators();
    let proof = checkpoint_proof(&state, &set, 42);

    assert_eq!(
        store
            .save_with_checkpoint_proof(&state, &set, Some(&proof))
            .unwrap(),
        1
    );

    let restored = store.load().unwrap().unwrap();
    assert_eq!(restored.public_checkpoint_proof, Some(proof.clone()));
    assert_eq!(restored.checkpoint_floor_epoch, 0);

    let view = second::PublicCurrencyView::new(
        restored.state.public_currency_summary(),
        restored.state.public_currency_states(),
    )
    .unwrap();
    let certified = restored
        .public_checkpoint_proof
        .unwrap()
        .verify(&view, &restored.validator_set)
        .unwrap();

    assert_eq!(certified.checkpoint().epoch(), 42);
    assert_eq!(certified.certificate().vote_count(), 3);

    store.remove_files().unwrap();
}

#[test]
fn snapshot_refuses_checkpoint_proof_for_a_different_public_state() {
    let base = temp_base("checkpoint-state-mismatch");
    let store = StateStore::new(&base);
    let state = SecondState::genesis([], 1).with_reserve(2).unwrap();
    let other_state = SecondState::genesis([], 10).with_reserve(2).unwrap();
    let set = validators();
    let proof = checkpoint_proof(&other_state, &set, 1);

    assert_eq!(
        store.save_with_checkpoint_proof(&state, &set, Some(&proof)),
        Err(second::PersistenceError::CheckpointDoesNotMatchState)
    );
    assert!(store.load().unwrap().is_none());
}

#[test]
fn snapshot_refuses_checkpoint_proof_for_a_different_validator_set_version() {
    let base = temp_base("checkpoint-set-mismatch");
    let store = StateStore::new(&base);
    let state = SecondState::genesis([], 1).with_reserve(2).unwrap();
    let set = validators();
    let valid = checkpoint_proof(&state, &set, 1);
    let proof = PublicCurrencyCheckpointProof::new(
        valid.checkpoint().clone(),
        set.version() + 1,
        valid.votes().to_vec(),
    );

    assert_eq!(
        store.save_with_checkpoint_proof(&state, &set, Some(&proof)),
        Err(second::PersistenceError::CheckpointValidatorSetMismatch {
            expected: set.version(),
            actual: set.version() + 1,
        })
    );
    assert!(store.load().unwrap().is_none());
}

#[test]
fn snapshot_checksum_does_not_turn_an_invalid_signature_into_a_certified_checkpoint() {
    let base = temp_base("checkpoint-invalid-signature");
    let store = StateStore::new(&base);
    let state = SecondState::genesis([], 1).with_reserve(2).unwrap();
    let set = validators();
    let checkpoint =
        PublicCurrencyCheckpoint::new(CURRENT_PROTOCOL_VERSION, 1, state.public_currency_summary());
    let proof = PublicCurrencyCheckpointProof::new(
        checkpoint,
        set.version(),
        vec![
            ValidatorVote::from_untrusted_parts(ValidatorId::new(1), [0; 64]),
            ValidatorVote::from_untrusted_parts(ValidatorId::new(2), [0; 64]),
            ValidatorVote::from_untrusted_parts(ValidatorId::new(3), [0; 64]),
        ],
    );

    store
        .save_with_checkpoint_proof(&state, &set, Some(&proof))
        .unwrap();

    let restored = store.load().unwrap().unwrap();
    let view = second::PublicCurrencyView::new(
        restored.state.public_currency_summary(),
        restored.state.public_currency_states(),
    )
    .unwrap();

    assert!(matches!(
        restored
            .public_checkpoint_proof
            .unwrap()
            .verify(&view, &restored.validator_set),
        Err(second::PublicCheckpointError::Finality(
            second::FinalityError::InvalidSignature(_)
        ))
    ));

    store.remove_files().unwrap();
}

#[test]
fn certified_checkpoint_floor_survives_state_advance_and_rejects_older_checkpoint() {
    let base = temp_base("checkpoint-floor");
    let store = StateStore::new(&base);
    let alice = support::account(1);
    let mut state = SecondState::genesis([alice], 1);
    let set = validators();

    let checkpoint = PublicCurrencyCheckpoint::new(
        CURRENT_PROTOCOL_VERSION,
        42,
        state.public_currency_summary(),
    );
    let statement = checkpoint.finality_statement(set.version());
    let votes = [1_u64, 2, 3]
        .into_iter()
        .map(|id| signed_vote(&statement, ValidatorId::new(id), &key(id as u8)))
        .collect();
    let certified =
        second::CertifiedPublicCurrencyCheckpoint::new(checkpoint, votes, &set).unwrap();

    store
        .save_with_certified_checkpoint(&state, &set, &certified)
        .unwrap();

    let restored = store.load().unwrap().unwrap();
    assert_eq!(restored.checkpoint_floor_epoch, 42);
    assert!(restored.public_checkpoint_proof.is_some());

    state
        .execute_finalized(
            &verified_task(
                1,
                vec![Operation::Issue {
                    account: alice,
                    count: 1,
                }],
            ),
            1,
        )
        .unwrap();
    store.save(&state, &set).unwrap();

    let advanced = store.load().unwrap().unwrap();
    assert_eq!(advanced.checkpoint_floor_epoch, 42);
    assert!(advanced.public_checkpoint_proof.is_none());

    let stale_checkpoint = PublicCurrencyCheckpoint::new(
        CURRENT_PROTOCOL_VERSION,
        41,
        state.public_currency_summary(),
    );
    let stale_statement = stale_checkpoint.finality_statement(set.version());
    let stale_votes = [1_u64, 2, 3]
        .into_iter()
        .map(|id| signed_vote(&stale_statement, ValidatorId::new(id), &key(id as u8)))
        .collect();
    let stale = second::CertifiedPublicCurrencyCheckpoint::new(stale_checkpoint, stale_votes, &set)
        .unwrap();

    assert_eq!(
        store.save_with_certified_checkpoint(&state, &set, &stale),
        Err(second::PersistenceError::StaleCheckpointEpoch {
            minimum: 42,
            actual: 41,
        })
    );

    store.remove_files().unwrap();
}

#[test]
fn snapshot_restores_private_business_state_protocol_state_and_validator_set() {
    let base = temp_base("restore");
    let store = StateStore::new(&base);

    let alice = support::account(1);
    let bob = support::account(2);
    let mut state = SecondState::genesis([alice, bob], 100)
        .with_reserve(2)
        .unwrap();
    register_payment_addresses(&mut state, [alice, bob]);

    let issue = verified_task(
        1,
        vec![Operation::Issue {
            account: alice,
            count: 3,
        }],
    );
    let issue_request_digest = issue.request_digest();
    state.execute_finalized(&issue, 1).unwrap();

    let transfer = verified_task(
        2,
        vec![Operation::Transfer {
            source: payment_address(alice),
            destination: payment_address(bob),
            amount: 1,
        }],
    );
    state.execute_finalized(&transfer, 2).unwrap();

    let generation = store.save(&state, &validators()).unwrap();
    assert_eq!(generation, 1);

    let PersistedNodeState {
        state: restored,
        validator_set,
        public_checkpoint_proof,
        generation,
        ..
    } = store.load().unwrap().unwrap();

    assert_eq!(generation, 1);
    assert!(public_checkpoint_proof.is_none());
    assert_eq!(restored.balance(alice), 2);
    assert_eq!(restored.balance(bob), 1);
    assert_eq!(restored.current_supply(), 5);
    assert_eq!(restored.reserve_count(), 2);
    assert_eq!(restored.next_currency_address(), 105);
    assert_eq!(
        restored.bound_request_digest(support::task_id(1)),
        Some(issue_request_digest)
    );
    assert_eq!(validator_set.version(), 7);
    assert_eq!(validator_set.len(), 4);
    assert_eq!(validator_set.quorum_threshold(), 3);

    store.remove_files().unwrap();
}

#[test]
fn newer_snapshot_wins_and_generation_increments() {
    let base = temp_base("generation");
    let store = StateStore::new(&base);

    let alice = support::account(1);
    let mut state = SecondState::genesis([alice], 1);
    let set = validators();

    assert_eq!(store.save(&state, &set).unwrap(), 1);

    let issue = verified_task(
        1,
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );
    state.execute_finalized(&issue, 1).unwrap();

    assert_eq!(store.save(&state, &set).unwrap(), 2);

    let restored = store.load().unwrap().unwrap();
    assert_eq!(restored.generation, 2);
    assert_eq!(restored.state.balance(alice), 1);

    store.remove_files().unwrap();
}

#[test]
fn durable_primary_commit_survives_mirror_write_failure() {
    let base = temp_base("mirror-write-failure");
    let store = StateStore::new(&base);
    let alice = support::account(1);
    let mut state = SecondState::genesis([alice], 1);
    let set = validators();

    assert_eq!(store.save(&state, &set).unwrap(), 1);

    let blocked_mirror = store.slot_path_for_generation(1);
    fs::remove_file(&blocked_mirror).unwrap();
    fs::create_dir(&blocked_mirror).unwrap();

    state
        .execute_finalized(
            &verified_task(
                1,
                vec![Operation::Issue {
                    account: alice,
                    count: 1,
                }],
            ),
            1,
        )
        .unwrap();

    assert_eq!(store.save(&state, &set).unwrap(), 2);

    let restored = store.load().unwrap().unwrap();
    assert_eq!(restored.generation, 2);
    assert_eq!(restored.state.balance(alice), 1);

    fs::remove_dir(&blocked_mirror).unwrap();
    store.remove_files().unwrap();
}

#[test]
fn corrupted_primary_slot_recovers_the_same_committed_snapshot_from_mirror() {
    let base = temp_base("fallback");
    let store = StateStore::new(&base);

    let alice = support::account(1);
    let mut state = SecondState::genesis([alice], 1);
    let set = validators();

    assert_eq!(store.save(&state, &set).unwrap(), 1);

    let issue = verified_task(
        1,
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );
    state.execute_finalized(&issue, 1).unwrap();
    assert_eq!(store.save(&state, &set).unwrap(), 2);

    let newest = store.slot_path_for_generation(2);
    fs::write(&newest, b"corrupted").unwrap();

    let restored = store.load().unwrap().unwrap();
    assert_eq!(restored.generation, 2);
    assert_eq!(restored.state.balance(alice), 1);

    store.remove_files().unwrap();
}

#[test]
fn same_generation_with_different_valid_contents_is_rejected() {
    let first_base = temp_base("split-generation-first");
    let second_base = temp_base("split-generation-second");
    let first_store = StateStore::new(&first_base);
    let second_store = StateStore::new(&second_base);
    let alice = support::account(1);
    let set = validators();

    let first_state = SecondState::genesis([alice], 1);
    first_store.save(&first_state, &set).unwrap();

    let mut second_state = SecondState::genesis([alice], 1);
    second_state
        .execute_finalized(
            &verified_task(
                1,
                vec![Operation::Issue {
                    account: alice,
                    count: 1,
                }],
            ),
            1,
        )
        .unwrap();
    second_store.save(&second_state, &set).unwrap();

    let divergent = fs::read(second_store.slot_path_for_generation(1)).unwrap();
    fs::write(first_store.slot_path_for_generation(2), divergent).unwrap();

    assert_eq!(
        first_store.load().err(),
        Some(second::PersistenceError::ConflictingSnapshotGeneration(1))
    );

    first_store.remove_files().unwrap();
    second_store.remove_files().unwrap();
}

#[test]
fn no_snapshot_returns_none_instead_of_inventing_state() {
    let base = temp_base("empty");
    let store = StateStore::new(&base);

    assert!(store.load().unwrap().is_none());
}

#[test]
fn two_corrupted_slots_are_reported_and_save_refuses_to_reset_state() {
    let base = temp_base("double-corruption");
    let store = StateStore::new(&base);

    let alice = support::account(1);
    let mut state = SecondState::genesis([alice], 1);
    let set = validators();

    store.save(&state, &set).unwrap();
    let issue = verified_task(
        1,
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );
    state.execute_finalized(&issue, 1).unwrap();
    store.save(&state, &set).unwrap();

    fs::write(store.slot_path_for_generation(1), b"broken-a").unwrap();
    fs::write(store.slot_path_for_generation(2), b"broken-b").unwrap();

    assert!(matches!(
        store.load(),
        Err(second::PersistenceError::NoValidSnapshot)
    ));
    assert_eq!(
        store.save(&state, &set),
        Err(second::PersistenceError::NoValidSnapshot)
    );

    store.remove_files().unwrap();
}
