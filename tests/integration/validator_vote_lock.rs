use crate::support;
use support::FinalizedExecute as _;

use second::{
    AuthorizerSet, CURRENT_PROTOCOL_VERSION, LegalTask, LegalTaskPayload, Operation,
    PreparationError, PreparedTaskBook, PublicCurrencyCheckpoint, SecondState, StateStore,
    ValidatorAdmissionRequest, ValidatorCredential, ValidatorId, ValidatorRegistry, ValidatorSet,
    ValidatorSetTransition, ValidatorSigner, ValidatorSigningError,
};
use support::{key, payment_address, register_payment_addresses, temp_base, verified_task};

fn validator_credential(id: u64) -> ValidatorCredential {
    ValidatorCredential::new(
        ValidatorId::new(id),
        key((id as u8).wrapping_add(40)).verifying_key().to_bytes(),
        key(id as u8).verifying_key().to_bytes(),
        key((id as u8).wrapping_add(80)).verifying_key().to_bytes(),
    )
    .unwrap()
}

fn validators() -> ValidatorSet {
    ValidatorSet::new(7, (1..=4).map(validator_credential)).unwrap()
}

fn transition_with_candidate(
    current: &ValidatorSet,
    registry: &ValidatorRegistry,
    candidate_id: u64,
    key_seed: u8,
) -> ValidatorSetTransition {
    let identity_key = key(key_seed);
    let consensus_key = key(key_seed.wrapping_add(1));
    let recovery_key = key(key_seed.wrapping_add(2));
    let credential = ValidatorCredential::new(
        ValidatorId::new(candidate_id),
        identity_key.verifying_key().to_bytes(),
        consensus_key.verifying_key().to_bytes(),
        recovery_key.verifying_key().to_bytes(),
    )
    .unwrap();
    let admission = ValidatorAdmissionRequest::sign(
        CURRENT_PROTOCOL_VERSION,
        credential.clone(),
        &identity_key,
        &consensus_key,
        &recovery_key,
    )
    .unwrap()
    .verify()
    .unwrap();

    let mut credentials = (1..=4).map(validator_credential).collect::<Vec<_>>();
    credentials.push(credential);
    let next = ValidatorSet::new(current.version() + 1, credentials).unwrap();

    ValidatorSetTransition::new(
        CURRENT_PROTOCOL_VERSION,
        20,
        current,
        registry,
        next,
        vec![admission],
        Vec::new(),
    )
    .unwrap()
}

#[test]
fn voting_task_cannot_be_cancelled_and_phase_survives_restart() {
    let alice = support::account(1);
    let store = StateStore::new(temp_base("vote-lock-never-cancel"));
    let set = validators();
    let mut state = SecondState::genesis([alice], 1);
    let mut prepared = PreparedTaskBook::new(store.clone()).unwrap();
    let task = verified_task(
        10,
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );

    prepared.prepare(&mut state, &task, 1, &set).unwrap();
    let digest = prepared.prepared_plan_digest(task.task_id()).unwrap();
    let first_vote = prepared
        .sign_prepared_vote(task.task_id(), ValidatorId::new(1), &key(1), &set)
        .unwrap();
    let repeated_vote = prepared
        .sign_prepared_vote(task.task_id(), ValidatorId::new(1), &key(1), &set)
        .unwrap();

    assert_eq!(first_vote, repeated_vote);
    assert_eq!(
        ValidatorSigner::new(ValidatorId::new(1), key(1), store.clone())
            .prepared_task_lock(task.task_id())
            .unwrap(),
        Some(digest)
    );
    assert_eq!(
        prepared.cancel(task.task_id()),
        Err(PreparationError::CancellationClosed(task.task_id()))
    );

    drop(prepared);

    let mut recovered = PreparedTaskBook::new(store.clone()).unwrap();
    assert!(recovered.is_prepared(task.task_id()));
    assert_eq!(
        recovered.cancel(task.task_id()),
        Err(PreparationError::CancellationClosed(task.task_id()))
    );

    store.remove_files().unwrap();
}

#[test]
fn restart_restores_the_exact_prepared_plan_and_repeats_the_same_vote() {
    let alice = support::account(1);
    let bob = support::account(2);
    let store = StateStore::new(temp_base("vote-lock-restart"));
    let set = validators();
    let task = verified_task(
        10,
        vec![Operation::Transfer {
            source: payment_address(alice),
            destination: payment_address(bob),
            amount: 1,
        }],
    );

    let (first_digest, first_vote);
    {
        let mut state = SecondState::genesis([alice, bob], 1);
        register_payment_addresses(&mut state, [alice, bob]);
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

        let mut prepared = PreparedTaskBook::new(store.clone()).unwrap();
        prepared.prepare(&mut state, &task, 2, &set).unwrap();
        first_digest = prepared.prepared_plan_digest(task.task_id()).unwrap();
        first_vote = prepared
            .sign_prepared_vote(task.task_id(), ValidatorId::new(4), &key(4), &set)
            .unwrap();
    }

    let restored = store.load().unwrap().unwrap();
    let mut prepared = PreparedTaskBook::new(store.clone()).unwrap();

    assert!(prepared.is_prepared(task.task_id()));
    assert_eq!(
        prepared.prepared_plan_digest(task.task_id()).unwrap(),
        first_digest
    );
    assert_eq!(prepared.claimed_currency_count(), 1);

    let repeated_vote = prepared
        .sign_prepared_vote(task.task_id(), ValidatorId::new(4), &key(4), &set)
        .unwrap();
    assert_eq!(repeated_vote, first_vote);
    assert_eq!(restored.state.next_currency_address(), 2);

    store.remove_files().unwrap();
}

#[test]
fn vote_lock_is_durable_before_vote_is_returned() {
    let alice = support::account(1);
    let store = StateStore::new(temp_base("vote-lock-durable"));
    let set = validators();
    let mut state = SecondState::genesis([alice], 1);
    let mut prepared = PreparedTaskBook::new(store.clone()).unwrap();
    let task = verified_task(
        10,
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );

    prepared.prepare(&mut state, &task, 1, &set).unwrap();
    let digest = prepared.prepared_plan_digest(task.task_id()).unwrap();
    prepared
        .sign_prepared_vote(task.task_id(), ValidatorId::new(2), &key(2), &set)
        .unwrap();

    assert_eq!(
        ValidatorSigner::new(ValidatorId::new(2), key(2), store.clone())
            .prepared_task_lock(task.task_id())
            .unwrap(),
        Some(digest)
    );

    store.remove_files().unwrap();
}

#[test]
fn prepared_before_expiry_can_be_voted_after_expiry() {
    let alice = support::account(1);
    let store = StateStore::new(temp_base("vote-lock-crosses-expiry"));
    let set = validators();
    let mut state = SecondState::genesis([alice], 1);
    let mut prepared = PreparedTaskBook::new(store.clone()).unwrap();

    let signing = key(9);
    let authorizers = AuthorizerSet::new(
        CURRENT_PROTOCOL_VERSION,
        [signing.verifying_key().to_bytes()],
    )
    .unwrap();
    let task = LegalTask::sign(
        LegalTaskPayload::new(
            support::task_id(10),
            CURRENT_PROTOCOL_VERSION,
            Some(5),
            vec![Operation::Issue {
                account: alice,
                count: 1,
            }],
        ),
        &signing,
    )
    .unwrap()
    .verify(&authorizers)
    .unwrap();

    prepared.prepare(&mut state, &task, 4, &set).unwrap();
    let digest = prepared.prepared_plan_digest(task.task_id()).unwrap();

    prepared
        .sign_prepared_vote(task.task_id(), ValidatorId::new(1), &key(1), &set)
        .unwrap();

    assert_eq!(
        ValidatorSigner::new(ValidatorId::new(1), key(1), store.clone())
            .prepared_task_lock(task.task_id())
            .unwrap(),
        Some(digest)
    );

    store.remove_files().unwrap();
}

#[test]
fn public_checkpoint_vote_lock_survives_restart_and_blocks_conflicting_digest() {
    let store = StateStore::new(temp_base("checkpoint-vote-lock"));
    let set = validators();
    let validator_id = ValidatorId::new(1);
    let first_state = SecondState::genesis([], 1).with_reserve(1).unwrap();
    let second_state = SecondState::genesis([], 1).with_reserve(2).unwrap();
    store.save(&first_state, &set).unwrap();
    let first = PublicCurrencyCheckpoint::new(
        CURRENT_PROTOCOL_VERSION,
        10,
        first_state.public_currency_summary(),
    );
    let conflicting = PublicCurrencyCheckpoint::new(
        CURRENT_PROTOCOL_VERSION,
        10,
        second_state.public_currency_summary(),
    );

    ValidatorSigner::new(validator_id, key(1), store.clone())
        .sign_public_checkpoint(&first, &set)
        .unwrap();

    let recovered = ValidatorSigner::new(validator_id, key(1), store.clone());
    assert!(matches!(
        recovered.sign_public_checkpoint(&conflicting, &set),
        Err(ValidatorSigningError::VoteLocked {
            validator_id: locked_validator,
            ..
        }) if locked_validator == validator_id
    ));

    store.remove_files().unwrap();
}

#[test]
fn validator_set_transition_vote_lock_survives_restart_and_blocks_conflicting_next_set() {
    let store = StateStore::new(temp_base("validator-transition-vote-lock"));
    let set = validators();
    let registry = ValidatorRegistry::from_validator_set(&set).unwrap();
    let state = SecondState::genesis([], 1);
    store.save(&state, &set).unwrap();

    let first = transition_with_candidate(&set, &registry, 5, 100);
    let conflicting = transition_with_candidate(&set, &registry, 6, 110);
    let validator_id = ValidatorId::new(1);

    let first_vote = ValidatorSigner::new(validator_id, key(1), store.clone())
        .sign_validator_set_transition(&first, &set)
        .unwrap();

    let recovered = ValidatorSigner::new(validator_id, key(1), store.clone());
    assert_eq!(
        recovered
            .sign_validator_set_transition(&first, &set)
            .unwrap(),
        first_vote
    );
    assert!(matches!(
        recovered.sign_validator_set_transition(&conflicting, &set),
        Err(ValidatorSigningError::VoteLocked {
            validator_id: locked_validator,
            ..
        }) if locked_validator == validator_id
    ));

    store.remove_files().unwrap();
}

#[test]
fn wrong_consensus_key_does_not_create_a_vote_lock() {
    let alice = support::account(1);
    let store = StateStore::new(temp_base("vote-lock-wrong-key"));
    let set = validators();
    let mut state = SecondState::genesis([alice], 1);
    let mut prepared = PreparedTaskBook::new(store.clone()).unwrap();
    let task = verified_task(
        10,
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );

    prepared.prepare(&mut state, &task, 1, &set).unwrap();

    assert_eq!(
        prepared.sign_prepared_vote(task.task_id(), ValidatorId::new(1), &key(2), &set),
        Err(PreparationError::Signing(
            ValidatorSigningError::ConsensusSigningKeyMismatch(ValidatorId::new(1))
        ))
    );
    assert_eq!(
        ValidatorSigner::new(ValidatorId::new(1), key(1), store.clone())
            .prepared_task_lock(task.task_id())
            .unwrap(),
        None
    );

    store.remove_files().unwrap();
}
