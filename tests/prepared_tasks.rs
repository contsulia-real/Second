use ed25519_dalek::SigningKey;
use second::{
    AccountAddress, AuthorizerSet, CURRENT_PROTOCOL_VERSION, ClaimError, CurrencyAddress,
    ExecutionError, ExecutionOutcome, LegalTask, LegalTaskPayload, Operation, PreparationError,
    PreparationOutcome, PreparedTaskBook, SecondState, StateStore, TaskId, ValidatorCredential,
    ValidatorId, ValidatorSet,
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

fn temp_base(name: &str) -> std::path::PathBuf {
    let unique = format!(
        "second-prepared-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    std::env::temp_dir().join(unique)
}

struct Harness {
    book: PreparedTaskBook,
    validators: ValidatorSet,
    store: StateStore,
}

impl Harness {
    fn new(name: &str) -> Self {
        let store = StateStore::new(temp_base(name));
        Self {
            book: PreparedTaskBook::new(store.clone()),
            validators: validators(),
            store,
        }
    }

    fn prepare(
        &mut self,
        state: &mut SecondState,
        task: &second::VerifiedLegalTask,
        now: u64,
    ) -> Result<PreparationOutcome, PreparationError> {
        self.book.prepare(state, task, now, &self.validators)
    }

    fn commit(
        &mut self,
        state: &mut SecondState,
        task_id: TaskId,
        now: u64,
    ) -> Result<ExecutionOutcome, PreparationError> {
        self.book.commit(state, task_id, now, &self.validators)
    }

    fn cancel(&mut self, task_id: TaskId) -> Result<(), PreparationError> {
        self.book.cancel(task_id)
    }

    fn claimed_currency_count(&self) -> usize {
        self.book.claimed_currency_count()
    }

    fn prepared_count(&self) -> usize {
        self.book.prepared_count()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.store.remove_files();
    }
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

fn expiring_task(
    task_id: u128,
    expires_at: u64,
    operations: Vec<Operation>,
) -> second::VerifiedLegalTask {
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
            Some(expires_at),
            operations,
        ),
        &signing,
    )
    .unwrap()
    .verify(&authorizers)
    .unwrap()
}

#[test]
fn prepared_issue_reserves_addresses_without_exposing_currency_before_commit() {
    let alice = AccountAddress::new(1);
    let mut state = SecondState::genesis([alice], 1);
    let mut prepared = Harness::new("case");
    let task = verified_task(
        10,
        vec![Operation::Issue {
            account: alice,
            count: 2,
        }],
    );

    assert_eq!(
        prepared.prepare(&mut state, &task, 1).unwrap(),
        PreparationOutcome::Prepared
    );

    assert_eq!(state.next_currency_address(), 3);
    assert_eq!(state.current_supply(), 0);
    assert!(!state.currency_exists(CurrencyAddress::new(1)));
    assert!(!state.currency_exists(CurrencyAddress::new(2)));

    assert_eq!(
        prepared.commit(&mut state, task.task_id(), 1).unwrap(),
        ExecutionOutcome::Succeeded
    );

    assert_eq!(state.next_currency_address(), 3);
    assert_eq!(state.current_supply(), 2);
    assert!(state.currency_exists(CurrencyAddress::new(1)));
    assert!(state.currency_exists(CurrencyAddress::new(2)));
}

#[test]
fn cancelling_prepared_issue_burns_reserved_addresses_forever() {
    let alice = AccountAddress::new(1);
    let mut state = SecondState::genesis([alice], 1);
    let mut prepared = Harness::new("case");
    let cancelled = verified_task(
        10,
        vec![Operation::Issue {
            account: alice,
            count: 2,
        }],
    );

    prepared.prepare(&mut state, &cancelled, 1).unwrap();
    prepared.cancel(cancelled.task_id()).unwrap();

    assert_eq!(state.next_currency_address(), 3);
    assert_eq!(state.current_supply(), 0);
    assert_eq!(prepared.claimed_currency_count(), 0);

    let later = verified_task(
        11,
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );
    state.execute(&later, 2).unwrap();

    assert_eq!(state.next_currency_address(), 4);
    assert!(!state.currency_exists(CurrencyAddress::new(1)));
    assert!(!state.currency_exists(CurrencyAddress::new(2)));
    assert!(state.currency_exists(CurrencyAddress::new(3)));
}

#[test]
fn cancelling_prepared_leak_repair_burns_replacement_reserve_addresses() {
    let alice = AccountAddress::new(1);
    let mut state = SecondState::genesis([alice], 1).with_reserve(1).unwrap();
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

    assert_eq!(state.next_currency_address(), 3);

    let repair = verified_task(
        10,
        vec![Operation::LeakRepair {
            leaked: vec![CurrencyAddress::new(2)],
        }],
    );
    let mut prepared = Harness::new("case");

    prepared.prepare(&mut state, &repair, 2).unwrap();
    assert_eq!(state.next_currency_address(), 4);
    assert_eq!(state.reserve_count(), 1);
    assert!(state.currency_exists(CurrencyAddress::new(2)));

    prepared.cancel(repair.task_id()).unwrap();

    assert_eq!(state.next_currency_address(), 4);
    assert_eq!(state.reserve_count(), 1);
    assert!(state.currency_exists(CurrencyAddress::new(2)));
    assert!(!state.currency_exists(CurrencyAddress::new(3)));
}

#[test]
fn prepared_transfer_holds_claim_until_cancel_or_commit() {
    let alice = AccountAddress::new(1);
    let bob = AccountAddress::new(2);
    let mut state = SecondState::genesis([alice, bob], 1);
    state
        .execute(
            &verified_task(
                1,
                vec![Operation::Issue {
                    account: alice,
                    count: 2,
                }],
            ),
            1,
        )
        .unwrap();

    let first = verified_task(
        10,
        vec![Operation::Transfer {
            source: alice,
            destination: bob,
            amount: 2,
        }],
    );
    let second = verified_task(
        11,
        vec![Operation::Transfer {
            source: alice,
            destination: bob,
            amount: 1,
        }],
    );
    let mut prepared = Harness::new("case");

    prepared.prepare(&mut state, &first, 2).unwrap();
    assert_eq!(prepared.claimed_currency_count(), 2);

    assert_eq!(
        prepared.prepare(&mut state, &second, 2),
        Err(PreparationError::Claim(ClaimError::CurrencyContention {
            account: alice,
            required: 1,
            available_unclaimed: 0,
            claimed_elsewhere: 2,
        }))
    );

    prepared.cancel(first.task_id()).unwrap();
    assert_eq!(prepared.claimed_currency_count(), 0);

    assert_eq!(
        prepared.prepare(&mut state, &second, 2).unwrap(),
        PreparationOutcome::Prepared
    );
}

#[test]
fn same_task_cannot_be_prepared_twice_at_the_same_time() {
    let alice = AccountAddress::new(1);
    let mut state = SecondState::genesis([alice], 1);
    let mut prepared = Harness::new("case");
    let task = verified_task(
        10,
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );

    prepared.prepare(&mut state, &task, 1).unwrap();

    assert_eq!(
        prepared.prepare(&mut state, &task, 1),
        Err(PreparationError::AlreadyPrepared(task.task_id()))
    );
    assert_eq!(state.next_currency_address(), 2);
}

#[test]
fn retry_after_cancel_uses_fresh_addresses_and_does_not_reuse_burned_range() {
    let alice = AccountAddress::new(1);
    let mut state = SecondState::genesis([alice], 1);
    let mut prepared = Harness::new("case");
    let task = verified_task(
        10,
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );

    prepared.prepare(&mut state, &task, 1).unwrap();
    prepared.cancel(task.task_id()).unwrap();
    assert_eq!(state.next_currency_address(), 2);

    prepared.prepare(&mut state, &task, 2).unwrap();
    assert_eq!(state.next_currency_address(), 3);
    prepared.commit(&mut state, task.task_id(), 2).unwrap();

    assert!(!state.currency_exists(CurrencyAddress::new(1)));
    assert!(state.currency_exists(CurrencyAddress::new(2)));
}

#[test]
fn task_expiring_while_prepared_is_cancelled_without_reusing_reserved_addresses() {
    let alice = AccountAddress::new(1);
    let mut state = SecondState::genesis([alice], 1);
    let mut prepared = Harness::new("case");
    let task = expiring_task(
        10,
        5,
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );

    prepared.prepare(&mut state, &task, 4).unwrap();
    assert_eq!(state.next_currency_address(), 2);

    assert_eq!(
        prepared.commit(&mut state, task.task_id(), 6),
        Err(PreparationError::Execution(ExecutionError::TaskExpired))
    );

    assert_eq!(state.current_supply(), 0);
    assert_eq!(state.next_currency_address(), 2);
    assert_eq!(prepared.prepared_count(), 0);
}

#[test]
fn failed_prepare_burns_addresses_allocated_by_earlier_operations() {
    let alice = AccountAddress::new(1);
    let missing = AccountAddress::new(99);
    let mut state = SecondState::genesis([alice], 1);
    let mut prepared = Harness::new("case");
    let task = verified_task(
        10,
        vec![
            Operation::Issue {
                account: alice,
                count: 2,
            },
            Operation::Issue {
                account: missing,
                count: 1,
            },
        ],
    );

    assert_eq!(
        prepared.prepare(&mut state, &task, 1),
        Err(PreparationError::Execution(
            ExecutionError::AccountNotFound(missing)
        ))
    );

    assert_eq!(state.current_supply(), 0);
    assert_eq!(state.next_currency_address(), 3);
    assert_eq!(prepared.prepared_count(), 0);
}

#[test]
fn crash_after_prepare_restores_burned_frontier_without_exposing_reserved_currency() {
    let alice = AccountAddress::new(1);
    let store = StateStore::new(temp_base("crash-after-prepare"));
    let validator_set = validators();
    let task = verified_task(
        10,
        vec![Operation::Issue {
            account: alice,
            count: 2,
        }],
    );

    {
        let mut state = SecondState::genesis([alice], 1);
        let mut prepared = PreparedTaskBook::new(store.clone());

        prepared
            .prepare(&mut state, &task, 1, &validator_set)
            .unwrap();

        assert_eq!(state.next_currency_address(), 3);
        assert_eq!(state.current_supply(), 0);
    }

    let restored = store.load().unwrap().unwrap();
    assert_eq!(restored.state.next_currency_address(), 3);
    assert_eq!(restored.state.current_supply(), 0);
    assert!(!restored.state.currency_exists(CurrencyAddress::new(1)));
    assert!(!restored.state.currency_exists(CurrencyAddress::new(2)));

    let mut state = restored.state;
    let mut reprepared = PreparedTaskBook::new(store.clone());
    reprepared
        .prepare(&mut state, &task, 2, &validator_set)
        .unwrap();

    assert_eq!(state.next_currency_address(), 5);
    reprepared
        .commit(&mut state, task.task_id(), 2, &validator_set)
        .unwrap();

    assert!(!state.currency_exists(CurrencyAddress::new(1)));
    assert!(!state.currency_exists(CurrencyAddress::new(2)));
    assert!(state.currency_exists(CurrencyAddress::new(3)));
    assert!(state.currency_exists(CurrencyAddress::new(4)));

    store.remove_files().unwrap();
}

#[test]
fn committed_prepared_task_is_durable_before_commit_returns() {
    let alice = AccountAddress::new(1);
    let store = StateStore::new(temp_base("durable-commit"));
    let validator_set = validators();
    let task = verified_task(
        10,
        vec![Operation::Issue {
            account: alice,
            count: 2,
        }],
    );

    let mut state = SecondState::genesis([alice], 1);
    let mut prepared = PreparedTaskBook::new(store.clone());
    prepared
        .prepare(&mut state, &task, 1, &validator_set)
        .unwrap();
    prepared
        .commit(&mut state, task.task_id(), 1, &validator_set)
        .unwrap();

    let mut restored = store.load().unwrap().unwrap().state;
    assert_eq!(restored.current_supply(), 2);
    assert_eq!(restored.next_currency_address(), 3);
    assert!(restored.currency_exists(CurrencyAddress::new(1)));
    assert!(restored.currency_exists(CurrencyAddress::new(2)));

    assert_eq!(
        restored.execute(&task, 2).unwrap(),
        ExecutionOutcome::AlreadySucceeded
    );

    store.remove_files().unwrap();
}

#[test]
fn destroy_and_leak_repair_targets_are_claimed_during_preparation() {
    let alice = AccountAddress::new(1);
    let mut state = SecondState::genesis([alice], 1).with_reserve(1).unwrap();
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

    let transfer = verified_task(
        10,
        vec![Operation::Transfer {
            source: alice,
            destination: alice,
            amount: 1,
        }],
    );
    let repair = verified_task(
        11,
        vec![Operation::LeakRepair {
            leaked: vec![CurrencyAddress::new(2)],
        }],
    );
    let mut prepared = Harness::new("case");

    prepared.prepare(&mut state, &transfer, 2).unwrap();

    assert_eq!(
        prepared.prepare(&mut state, &repair, 2),
        Err(PreparationError::Claim(
            ClaimError::ExplicitCurrencyContention {
                currency: CurrencyAddress::new(2),
            }
        ))
    );
}
