use std::fs;

use ed25519_dalek::SigningKey;
use second::{
    AccountAddress, AuthorizerSet, CURRENT_PROTOCOL_VERSION, LegalTask, LegalTaskPayload,
    Operation, PersistedNodeState, SecondState, StateStore, TaskId, ValidatorCredential,
    ValidatorId, ValidatorSet,
};

fn key(byte: u8) -> SigningKey {
    SigningKey::from_bytes(&[byte; 32])
}

fn verified_task(task_id: u128, operations: Vec<Operation>) -> second::VerifiedLegalTask {
    let signing = key(9);
    let authorizers = AuthorizerSet::new(
        CURRENT_PROTOCOL_VERSION,
        [signing.verifying_key().to_bytes()],
    )
    .unwrap();
    let payload = LegalTaskPayload::new(
        TaskId::new(task_id),
        CURRENT_PROTOCOL_VERSION,
        None,
        operations,
    );

    LegalTask::sign(payload, &signing)
        .unwrap()
        .verify(&authorizers)
        .unwrap()
}

fn validators() -> ValidatorSet {
    ValidatorSet::new(
        7,
        (1..=4).map(|id| {
            ValidatorCredential::new(
                ValidatorId::new(id),
                key(id as u8).verifying_key().to_bytes(),
            )
            .unwrap()
        }),
    )
    .unwrap()
}

fn temp_base(name: &str) -> std::path::PathBuf {
    let unique = format!(
        "second-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    std::env::temp_dir().join(unique)
}

#[test]
fn snapshot_restores_private_business_state_protocol_state_and_validator_set() {
    let base = temp_base("restore");
    let store = StateStore::new(&base);

    let alice = AccountAddress::new(1);
    let bob = AccountAddress::new(2);
    let mut state = SecondState::genesis([alice, bob], 100)
        .with_reserve(2)
        .unwrap();

    let issue = verified_task(
        1,
        vec![Operation::Issue {
            account: alice,
            count: 3,
        }],
    );
    let issue_digest = issue.request_digest();
    state.execute(&issue, 1).unwrap();

    let transfer = verified_task(
        2,
        vec![Operation::Transfer {
            source: alice,
            destination: bob,
            amount: 1,
        }],
    );
    state.execute(&transfer, 2).unwrap();

    let generation = store.save(&state, &validators()).unwrap();
    assert_eq!(generation, 1);

    let PersistedNodeState {
        state: restored,
        validator_set,
        generation,
    } = store.load().unwrap().unwrap();

    assert_eq!(generation, 1);
    assert_eq!(restored.balance(alice), 2);
    assert_eq!(restored.balance(bob), 1);
    assert_eq!(restored.current_supply(), 5);
    assert_eq!(restored.reserve_count(), 2);
    assert_eq!(restored.next_currency_address(), 105);
    assert_eq!(
        restored.bound_request_digest(TaskId::new(1)),
        Some(issue_digest)
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

    let alice = AccountAddress::new(1);
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
    state.execute(&issue, 1).unwrap();

    assert_eq!(store.save(&state, &set).unwrap(), 2);

    let restored = store.load().unwrap().unwrap();
    assert_eq!(restored.generation, 2);
    assert_eq!(restored.state.balance(alice), 1);

    store.remove_files().unwrap();
}

#[test]
fn corrupted_newest_slot_falls_back_to_previous_valid_snapshot() {
    let base = temp_base("fallback");
    let store = StateStore::new(&base);

    let alice = AccountAddress::new(1);
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
    state.execute(&issue, 1).unwrap();
    assert_eq!(store.save(&state, &set).unwrap(), 2);

    let newest = store.slot_path_for_generation(2);
    fs::write(&newest, b"corrupted").unwrap();

    let restored = store.load().unwrap().unwrap();
    assert_eq!(restored.generation, 1);
    assert_eq!(restored.state.balance(alice), 0);

    store.remove_files().unwrap();
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

    let alice = AccountAddress::new(1);
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
    state.execute(&issue, 1).unwrap();
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
