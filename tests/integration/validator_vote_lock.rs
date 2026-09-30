use crate::support;
use support::FinalizedExecute as _;

use second::{
    AuthorizerSet, CURRENT_PROTOCOL_VERSION, LegalTask, LegalTaskPayload, Operation,
    PreparationError, PreparedTaskBook, SecondState, StateStore, ValidatorCredential, ValidatorId,
    ValidatorSet, ValidatorSigner, ValidatorSigningError,
};
use support::{key, payment_address, register_payment_addresses, temp_base, verified_task};

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

#[test]
fn validator_can_repeat_the_same_vote_but_cannot_sign_a_reprepared_plan() {
    let alice = support::account(1);
    let store = StateStore::new(temp_base("vote-lock-never-change"));
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
    let first_digest = prepared.prepared_plan_digest(task.task_id()).unwrap();
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
        Some(first_digest)
    );

    prepared.cancel(task.task_id()).unwrap();
    prepared.prepare(&mut state, &task, 2, &set).unwrap();
    let second_digest = prepared.prepared_plan_digest(task.task_id()).unwrap();
    assert_ne!(first_digest, second_digest);

    assert_eq!(
        prepared.sign_prepared_vote(task.task_id(), ValidatorId::new(1), &key(1), &set),
        Err(PreparationError::Signing(
            ValidatorSigningError::VoteLocked {
                validator_id: ValidatorId::new(1),
                locked_digest: first_digest,
                attempted_digest: second_digest,
            }
        ))
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
    let prepared = PreparedTaskBook::new(store.clone()).unwrap();

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
