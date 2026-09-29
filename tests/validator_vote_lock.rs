use ed25519_dalek::SigningKey;
use second::{
    AccountAddress, AuthorizerSet, CURRENT_PROTOCOL_VERSION, LegalTask, LegalTaskPayload,
    Operation, PreparationError, PreparationOutcome, PreparedTaskBook, SecondState, StateStore,
    TaskId, ValidatorCredential, ValidatorId, ValidatorSet,
};

fn key(byte: u8) -> SigningKey {
    SigningKey::from_bytes(&[byte; 32])
}

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

fn verified_task(task_id: u128, operations: Vec<Operation>) -> second::VerifiedLegalTask {
    let signing = key(9);
    let authorizers = AuthorizerSet::new(
        CURRENT_PROTOCOL_VERSION,
        [signing.verifying_key().to_bytes()],
    )
    .unwrap();

    LegalTask::sign(
        LegalTaskPayload::new(
            TaskId::new(task_id),
            CURRENT_PROTOCOL_VERSION,
            None,
            operations,
        ),
        &signing,
    )
    .unwrap()
    .verify(&authorizers)
    .unwrap()
}

fn temp_base(name: &str) -> std::path::PathBuf {
    let unique = format!(
        "second-vote-lock-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    std::env::temp_dir().join(unique)
}

#[test]
fn validator_can_repeat_the_same_vote_but_can_never_change_it() {
    let alice = AccountAddress::new(1);
    let store = StateStore::new(temp_base("never-change"));
    let set = validators();
    let mut state = SecondState::genesis([alice], 1);
    let mut prepared = PreparedTaskBook::new(store.clone());
    let task = verified_task(
        10,
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );

    assert_eq!(
        prepared.prepare(&mut state, &task, 1, &set).unwrap(),
        PreparationOutcome::Prepared
    );

    let first_digest = prepared.prepared_plan_digest(task.task_id()).unwrap();
    let first_vote = prepared
        .sign_prepared_vote(
            &mut state,
            task.task_id(),
            1,
            ValidatorId::new(1),
            &key(1),
            &set,
        )
        .unwrap();
    let repeated_vote = prepared
        .sign_prepared_vote(
            &mut state,
            task.task_id(),
            1,
            ValidatorId::new(1),
            &key(1),
            &set,
        )
        .unwrap();

    assert_eq!(first_vote, repeated_vote);
    assert_eq!(
        state.validator_vote_lock(ValidatorId::new(1), task.task_id()),
        Some(first_digest)
    );

    prepared.cancel(task.task_id()).unwrap();
    prepared.prepare(&mut state, &task, 2, &set).unwrap();
    let second_digest = prepared.prepared_plan_digest(task.task_id()).unwrap();
    assert_ne!(first_digest, second_digest);

    assert_eq!(
        prepared.sign_prepared_vote(
            &mut state,
            task.task_id(),
            1,
            ValidatorId::new(1),
            &key(1),
            &set,
        ),
        Err(PreparationError::ValidatorVoteLocked {
            validator_id: ValidatorId::new(1),
            task_id: task.task_id(),
            locked_digest: first_digest,
            attempted_digest: second_digest,
        })
    );

    store.remove_files().unwrap();
}

#[test]
fn vote_lock_is_durable_before_vote_is_returned() {
    let alice = AccountAddress::new(1);
    let store = StateStore::new(temp_base("durable"));
    let set = validators();
    let mut state = SecondState::genesis([alice], 1);
    let mut prepared = PreparedTaskBook::new(store.clone());
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
        .sign_prepared_vote(
            &mut state,
            task.task_id(),
            1,
            ValidatorId::new(2),
            &key(2),
            &set,
        )
        .unwrap();

    let restored = store.load().unwrap().unwrap();
    assert_eq!(
        restored
            .state
            .validator_vote_lock(ValidatorId::new(2), task.task_id()),
        Some(digest)
    );

    store.remove_files().unwrap();
}

#[test]
fn restart_does_not_unlock_validator_for_a_different_plan() {
    let alice = AccountAddress::new(1);
    let store = StateStore::new(temp_base("restart"));
    let set = validators();
    let task = verified_task(
        10,
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );

    let first_digest;
    {
        let mut state = SecondState::genesis([alice], 1);
        let mut prepared = PreparedTaskBook::new(store.clone());
        prepared.prepare(&mut state, &task, 1, &set).unwrap();
        first_digest = prepared.prepared_plan_digest(task.task_id()).unwrap();
        prepared
            .sign_prepared_vote(
                &mut state,
                task.task_id(),
                1,
                ValidatorId::new(3),
                &key(3),
                &set,
            )
            .unwrap();
    }

    let mut state = store.load().unwrap().unwrap().state;
    let mut prepared = PreparedTaskBook::new(store.clone());
    prepared.prepare(&mut state, &task, 2, &set).unwrap();
    let second_digest = prepared.prepared_plan_digest(task.task_id()).unwrap();
    assert_ne!(first_digest, second_digest);

    assert_eq!(
        prepared.sign_prepared_vote(
            &mut state,
            task.task_id(),
            1,
            ValidatorId::new(3),
            &key(3),
            &set,
        ),
        Err(PreparationError::ValidatorVoteLocked {
            validator_id: ValidatorId::new(3),
            task_id: task.task_id(),
            locked_digest: first_digest,
            attempted_digest: second_digest,
        })
    );

    store.remove_files().unwrap();
}

#[test]
fn validator_that_did_not_vote_old_plan_can_vote_new_plan() {
    let alice = AccountAddress::new(1);
    let store = StateStore::new(temp_base("other-validator"));
    let set = validators();
    let mut state = SecondState::genesis([alice], 1);
    let mut prepared = PreparedTaskBook::new(store.clone());
    let task = verified_task(
        10,
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );

    prepared.prepare(&mut state, &task, 1, &set).unwrap();
    prepared
        .sign_prepared_vote(
            &mut state,
            task.task_id(),
            1,
            ValidatorId::new(1),
            &key(1),
            &set,
        )
        .unwrap();
    prepared.cancel(task.task_id()).unwrap();

    prepared.prepare(&mut state, &task, 2, &set).unwrap();
    let new_digest = prepared.prepared_plan_digest(task.task_id()).unwrap();
    prepared
        .sign_prepared_vote(
            &mut state,
            task.task_id(),
            1,
            ValidatorId::new(2),
            &key(2),
            &set,
        )
        .unwrap();

    assert_eq!(
        state.validator_vote_lock(ValidatorId::new(2), task.task_id()),
        Some(new_digest)
    );

    store.remove_files().unwrap();
}

#[test]
fn same_validator_can_vote_for_a_different_task() {
    let alice = AccountAddress::new(1);
    let store = StateStore::new(temp_base("different-task"));
    let set = validators();
    let mut state = SecondState::genesis([alice], 1);
    let mut prepared = PreparedTaskBook::new(store.clone());

    let first = verified_task(
        10,
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );
    let second = verified_task(
        11,
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );

    prepared.prepare(&mut state, &first, 1, &set).unwrap();
    prepared
        .sign_prepared_vote(
            &mut state,
            first.task_id(),
            1,
            ValidatorId::new(1),
            &key(1),
            &set,
        )
        .unwrap();
    prepared.cancel(first.task_id()).unwrap();

    prepared.prepare(&mut state, &second, 2, &set).unwrap();
    prepared
        .sign_prepared_vote(
            &mut state,
            second.task_id(),
            2,
            ValidatorId::new(1),
            &key(1),
            &set,
        )
        .unwrap();

    assert!(
        state
            .validator_vote_lock(ValidatorId::new(1), first.task_id())
            .is_some()
    );
    assert!(
        state
            .validator_vote_lock(ValidatorId::new(1), second.task_id())
            .is_some()
    );

    store.remove_files().unwrap();
}

#[test]
fn restart_can_repeat_exactly_the_same_locked_vote() {
    let alice = AccountAddress::new(1);
    let bob = AccountAddress::new(2);
    let store = StateStore::new(temp_base("same-after-restart"));
    let set = validators();
    let task = verified_task(
        10,
        vec![Operation::Transfer {
            source: alice,
            destination: bob,
            amount: 1,
        }],
    );

    let first_vote;
    let first_digest;
    {
        let mut state = SecondState::genesis([alice, bob], 1);
        state
            .execute(
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

        let mut prepared = PreparedTaskBook::new(store.clone());
        prepared.prepare(&mut state, &task, 2, &set).unwrap();
        first_digest = prepared.prepared_plan_digest(task.task_id()).unwrap();
        first_vote = prepared
            .sign_prepared_vote(
                &mut state,
                task.task_id(),
                2,
                ValidatorId::new(4),
                &key(4),
                &set,
            )
            .unwrap();
    }

    let mut state = store.load().unwrap().unwrap().state;
    let mut prepared = PreparedTaskBook::new(store.clone());
    prepared.prepare(&mut state, &task, 3, &set).unwrap();
    assert_eq!(
        prepared.prepared_plan_digest(task.task_id()).unwrap(),
        first_digest
    );

    let repeated_vote = prepared
        .sign_prepared_vote(
            &mut state,
            task.task_id(),
            3,
            ValidatorId::new(4),
            &key(4),
            &set,
        )
        .unwrap();

    assert_eq!(repeated_vote, first_vote);

    store.remove_files().unwrap();
}

#[test]
fn expired_prepared_task_cannot_burn_a_validator_vote() {
    let alice = AccountAddress::new(1);
    let store = StateStore::new(temp_base("expired"));
    let set = validators();
    let mut state = SecondState::genesis([alice], 1);
    let mut prepared = PreparedTaskBook::new(store.clone());

    let signing = key(9);
    let authorizers = AuthorizerSet::new(
        CURRENT_PROTOCOL_VERSION,
        [signing.verifying_key().to_bytes()],
    )
    .unwrap();
    let task = LegalTask::sign(
        LegalTaskPayload::new(
            TaskId::new(10),
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

    assert_eq!(
        prepared.sign_prepared_vote(
            &mut state,
            task.task_id(),
            6,
            ValidatorId::new(1),
            &key(1),
            &set,
        ),
        Err(PreparationError::Execution(
            second::ExecutionError::TaskExpired
        ))
    );
    assert_eq!(
        state.validator_vote_lock(ValidatorId::new(1), task.task_id()),
        None
    );

    store.remove_files().unwrap();
}

#[test]
fn wrong_consensus_key_is_rejected_without_creating_vote_lock() {
    let alice = AccountAddress::new(1);
    let store = StateStore::new(temp_base("wrong-key"));
    let set = validators();
    let mut state = SecondState::genesis([alice], 1);
    let mut prepared = PreparedTaskBook::new(store.clone());
    let task = verified_task(
        10,
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );

    prepared.prepare(&mut state, &task, 1, &set).unwrap();

    assert_eq!(
        prepared.sign_prepared_vote(
            &mut state,
            task.task_id(),
            1,
            ValidatorId::new(1),
            &key(2),
            &set,
        ),
        Err(PreparationError::ConsensusSigningKeyMismatch(
            ValidatorId::new(1)
        ))
    );
    assert_eq!(
        state.validator_vote_lock(ValidatorId::new(1), task.task_id()),
        None
    );

    store.remove_files().unwrap();
}
