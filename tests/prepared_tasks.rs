use ed25519_dalek::SigningKey;
use second::{
    AccountAddress, AuthorizerSet, CURRENT_PROTOCOL_VERSION, ClaimError, CurrencyAddress,
    ExecutionError, ExecutionOutcome, FinalityCertificate, LegalTask, LegalTaskPayload, Operation,
    PreparationError, PreparationOutcome, PreparedTaskBook, SecondState, StateStore, TaskId,
    ValidatorCredential, ValidatorId, ValidatorSet, ValidatorVote,
};

fn key(byte: u8) -> SigningKey {
    SigningKey::from_bytes(&[byte; 32])
}

fn validators() -> ValidatorSet {
    validators_at(7)
}

fn validators_at(version: u64) -> ValidatorSet {
    ValidatorSet::new(
        version,
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
        let certificate = certify(&self.book, task_id, &self.validators);
        self.book
            .commit(state, task_id, now, &certificate, &self.validators)
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

fn certify(
    book: &PreparedTaskBook,
    task_id: TaskId,
    validator_set: &ValidatorSet,
) -> FinalityCertificate {
    let statement = book
        .prepared_finality_statement(task_id, validator_set)
        .unwrap();
    let votes = [1_u64, 2, 3]
        .into_iter()
        .map(|id| ValidatorVote::sign(&statement, ValidatorId::new(id), &key(id as u8)))
        .collect();

    FinalityCertificate::new(statement, votes, validator_set).unwrap()
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
fn stale_finality_certificate_cannot_commit_a_reprepared_task_with_new_addresses() {
    let alice = AccountAddress::new(1);
    let mut state = SecondState::genesis([alice], 1);
    let mut prepared = Harness::new("stale-certificate");
    let task = verified_task(
        10,
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );

    prepared.prepare(&mut state, &task, 1).unwrap();
    let old_digest = prepared.book.prepared_plan_digest(task.task_id()).unwrap();
    let old_certificate = certify(&prepared.book, task.task_id(), &prepared.validators);

    prepared.cancel(task.task_id()).unwrap();
    prepared.prepare(&mut state, &task, 2).unwrap();

    let new_digest = prepared.book.prepared_plan_digest(task.task_id()).unwrap();
    assert_ne!(old_digest, new_digest);

    assert_eq!(
        prepared.book.commit(
            &mut state,
            task.task_id(),
            2,
            &old_certificate,
            &prepared.validators,
        ),
        Err(PreparationError::FinalitySubjectMismatch {
            expected: new_digest,
            actual: old_digest,
        })
    );
    assert!(prepared.book.is_prepared(task.task_id()));

    let current_certificate = certify(&prepared.book, task.task_id(), &prepared.validators);
    assert_eq!(
        prepared
            .book
            .commit(
                &mut state,
                task.task_id(),
                2,
                &current_certificate,
                &prepared.validators,
            )
            .unwrap(),
        ExecutionOutcome::Succeeded
    );

    assert!(!state.currency_exists(CurrencyAddress::new(1)));
    assert!(state.currency_exists(CurrencyAddress::new(2)));
}

#[test]
fn prepared_plan_is_bound_to_the_validator_set_version_that_created_it() {
    let alice = AccountAddress::new(1);
    let mut state = SecondState::genesis([alice], 1);
    let mut prepared = Harness::new("validator-set-binding");
    let task = verified_task(
        10,
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );

    prepared.prepare(&mut state, &task, 1).unwrap();
    let next_set = validators_at(prepared.validators.version() + 1);

    assert_eq!(
        prepared
            .book
            .prepared_finality_statement(task.task_id(), &next_set),
        Err(PreparationError::ValidatorSetVersionChanged {
            expected: prepared.validators.version(),
            actual: next_set.version(),
        })
    );
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
    let certificate = certify(&reprepared, task.task_id(), &validator_set);
    reprepared
        .commit(&mut state, task.task_id(), 2, &certificate, &validator_set)
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
    let certificate = certify(&prepared, task.task_id(), &validator_set);
    prepared
        .commit(&mut state, task.task_id(), 1, &certificate, &validator_set)
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
