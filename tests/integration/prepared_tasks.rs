use crate::support;
use support::FinalizedExecute as _;

use second::{
    AuthorizerSet, CURRENT_PROTOCOL_VERSION, ClaimError, CurrencyAddress, ExecutionError,
    ExecutionOutcome, FinalityCertificate, LegalTask, LegalTaskPayload, Operation,
    PersistenceError, PreparationError, PreparationOutcome, PreparedTaskBook, SecondState,
    StateStore, TaskId, ValidatorId, ValidatorSet,
};
use support::{
    FinalityHarness, certificate_from_keys, key, payment_address, register_payment_addresses,
    temp_base, verified_task,
};

fn validators() -> ValidatorSet {
    validators_at(7)
}

fn validators_at(version: u64) -> ValidatorSet {
    support::validator_set(version, 1..=4)
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
        .map(|id| (ValidatorId::new(id), key((id * 3 + 1) as u8)));

    certificate_from_keys(statement, validator_set, votes)
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
            support::task_id(task_id),
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
    let alice = support::account(1);
    let mut state = SecondState::genesis([alice], 1);
    let mut prepared = FinalityHarness::new("case");
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
        prepared.commit(&mut state, task.task_id()).unwrap(),
        ExecutionOutcome::Succeeded
    );

    assert_eq!(state.next_currency_address(), 3);
    assert_eq!(state.current_supply(), 2);
    assert!(state.currency_exists(CurrencyAddress::new(1)));
    assert!(state.currency_exists(CurrencyAddress::new(2)));
}

#[test]
fn impossible_issue_allocation_fails_without_burning_currency_frontier() {
    let alice = support::account(1);
    let mut state = SecondState::genesis([alice], 0);
    let mut prepared = FinalityHarness::new("issue-allocation-failure");
    let task = verified_task(
        10,
        vec![Operation::Issue {
            account: alice,
            count: u64::MAX,
        }],
    );

    assert_eq!(
        prepared.prepare(&mut state, &task, 1),
        Err(PreparationError::Execution(
            ExecutionError::CurrencyAllocationFailed {
                requested: u64::MAX
            }
        ))
    );
    assert_eq!(state.next_currency_address(), 0);
    assert_eq!(state.current_supply(), 0);
    assert_eq!(prepared.prepared_count(), 0);
}

#[test]
fn cancelling_prepared_issue_burns_reserved_addresses_forever() {
    let alice = support::account(1);
    let mut state = SecondState::genesis([alice], 1);
    let mut prepared = FinalityHarness::new("case");
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
    state.execute_finalized(&later, 2).unwrap();

    assert_eq!(state.next_currency_address(), 4);
    assert!(!state.currency_exists(CurrencyAddress::new(1)));
    assert!(!state.currency_exists(CurrencyAddress::new(2)));
    assert!(state.currency_exists(CurrencyAddress::new(3)));
}

#[test]
fn cancelling_prepared_leak_repair_burns_replacement_reserve_addresses() {
    let alice = support::account(1);
    let mut state = SecondState::genesis([alice], 1).with_reserve(1).unwrap();
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

    assert_eq!(state.next_currency_address(), 3);

    let repair = verified_task(
        10,
        vec![Operation::LeakRepair {
            leaked: vec![CurrencyAddress::new(2)],
        }],
    );
    let mut prepared = FinalityHarness::new("case");

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
    let alice = support::account(1);
    let bob = support::account(2);
    let mut state = SecondState::genesis([alice, bob], 1);
    register_payment_addresses(&mut state, [alice, bob]);
    state
        .execute_finalized(
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
            source: payment_address(alice),
            destination: payment_address(bob),
            amount: 2,
        }],
    );
    let second = verified_task(
        11,
        vec![Operation::Transfer {
            source: payment_address(alice),
            destination: payment_address(bob),
            amount: 1,
        }],
    );
    let mut prepared = FinalityHarness::new("case");

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
fn valid_finality_apply_failure_keeps_prepared_plan_recoverable() {
    let alice = support::account(1);
    let bob = support::account(2);
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

    let mut stale_state = state.clone();
    let store = StateStore::new(temp_base("finality-apply-failure"));
    let validator_set = validators();
    let task = verified_task(
        10,
        vec![Operation::Transfer {
            source: payment_address(alice),
            destination: payment_address(bob),
            amount: 1,
        }],
    );
    let mut prepared = PreparedTaskBook::new(store.clone()).unwrap();

    prepared
        .prepare(&mut state, &task, 2, &validator_set)
        .unwrap();
    let certificate = certify(&prepared, task.task_id(), &validator_set);

    assert!(matches!(
        prepared.commit(
            &mut stale_state,
            task.task_id(),
            &certificate,
            &validator_set,
        ),
        Err(PreparationError::Execution(
            ExecutionError::TransferNotEstablished(_)
        ))
    ));
    assert!(prepared.is_prepared(task.task_id()));
    assert_eq!(prepared.claimed_currency_count(), 1);
    assert_eq!(stale_state.balance(alice), 1);
    assert_eq!(stale_state.balance(bob), 0);
    assert_eq!(
        prepared.cancel(task.task_id()),
        Err(PreparationError::CancellationClosed(task.task_id()))
    );

    drop(prepared);

    let mut recovered = PreparedTaskBook::new(store.clone()).unwrap();
    assert!(recovered.is_prepared(task.task_id()));
    assert_eq!(recovered.claimed_currency_count(), 1);
    assert_eq!(
        recovered.cancel(task.task_id()),
        Err(PreparationError::CancellationClosed(task.task_id()))
    );

    store.remove_files().unwrap();
}

#[test]
fn same_task_cannot_be_prepared_twice_at_the_same_time() {
    let alice = support::account(1);
    let mut state = SecondState::genesis([alice], 1);
    let mut prepared = FinalityHarness::new("case");
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
    let alice = support::account(1);
    let mut state = SecondState::genesis([alice], 1);
    let mut prepared = FinalityHarness::new("case");
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
    prepared.commit(&mut state, task.task_id()).unwrap();

    assert!(!state.currency_exists(CurrencyAddress::new(1)));
    assert!(state.currency_exists(CurrencyAddress::new(2)));
}

#[test]
fn stale_finality_certificate_cannot_commit_a_reprepared_task_with_new_addresses() {
    let alice = support::account(1);
    let mut state = SecondState::genesis([alice], 1);
    let mut prepared = FinalityHarness::new("stale-certificate");
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
    let alice = support::account(1);
    let mut state = SecondState::genesis([alice], 1);
    let mut prepared = FinalityHarness::new("validator-set-binding");
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
fn prepared_before_expiry_can_commit_after_expiry() {
    let alice = support::account(1);
    let mut state = SecondState::genesis([alice], 1);
    let mut prepared = FinalityHarness::new("prepared-crosses-expiry");
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
        prepared.commit(&mut state, task.task_id()).unwrap(),
        ExecutionOutcome::Succeeded
    );

    assert_eq!(state.current_supply(), 1);
    assert_eq!(state.next_currency_address(), 2);
    assert!(state.currency_exists(CurrencyAddress::new(1)));
    assert_eq!(prepared.prepared_count(), 0);
}

#[test]
fn failed_prepare_burns_addresses_allocated_by_earlier_operations() {
    let alice = support::account(1);
    let missing = support::account(99);
    let mut state = SecondState::genesis([alice], 1);
    let mut prepared = FinalityHarness::new("case");
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
fn crash_after_prepare_restores_the_exact_plan_without_reallocating_addresses() {
    let alice = support::account(1);
    let store = StateStore::new(temp_base("crash-after-prepare"));
    let validator_set = validators();
    let task = verified_task(
        10,
        vec![Operation::Issue {
            account: alice,
            count: 2,
        }],
    );

    let original_digest;
    {
        let mut state = SecondState::genesis([alice], 1);
        let mut prepared = PreparedTaskBook::new(store.clone()).unwrap();

        prepared
            .prepare(&mut state, &task, 1, &validator_set)
            .unwrap();

        original_digest = prepared.prepared_plan_digest(task.task_id()).unwrap();
        assert_eq!(state.next_currency_address(), 3);
        assert_eq!(state.current_supply(), 0);
    }

    let restored = store.load().unwrap().unwrap();
    assert_eq!(restored.state.next_currency_address(), 3);
    assert_eq!(restored.state.current_supply(), 0);
    assert!(!restored.state.currency_exists(CurrencyAddress::new(1)));
    assert!(!restored.state.currency_exists(CurrencyAddress::new(2)));

    let mut state = restored.state;
    let mut recovered = PreparedTaskBook::new(store.clone()).unwrap();
    assert!(recovered.is_prepared(task.task_id()));
    assert_eq!(
        recovered.prepared_plan_digest(task.task_id()).unwrap(),
        original_digest
    );

    let certificate = certify(&recovered, task.task_id(), &validator_set);
    recovered
        .commit(&mut state, task.task_id(), &certificate, &validator_set)
        .unwrap();

    assert_eq!(state.next_currency_address(), 3);
    assert!(state.currency_exists(CurrencyAddress::new(1)));
    assert!(state.currency_exists(CurrencyAddress::new(2)));

    store.remove_files().unwrap();
}

#[test]
fn committed_prepared_task_is_durable_before_commit_returns() {
    let alice = support::account(1);
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
    let mut prepared = PreparedTaskBook::new(store.clone()).unwrap();
    prepared
        .prepare(&mut state, &task, 1, &validator_set)
        .unwrap();
    let certificate = certify(&prepared, task.task_id(), &validator_set);
    prepared
        .commit(&mut state, task.task_id(), &certificate, &validator_set)
        .unwrap();

    let mut restored = store.load().unwrap().unwrap().state;
    assert_eq!(restored.current_supply(), 2);
    assert_eq!(restored.next_currency_address(), 3);
    assert!(restored.currency_exists(CurrencyAddress::new(1)));
    assert!(restored.currency_exists(CurrencyAddress::new(2)));

    assert_eq!(
        restored.execute_finalized(&task, 2).unwrap(),
        ExecutionOutcome::AlreadySucceeded
    );

    store.remove_files().unwrap();
}

#[test]
fn destroy_and_leak_repair_targets_are_claimed_during_preparation() {
    let alice = support::account(1);
    let mut state = SecondState::genesis([alice], 1).with_reserve(1).unwrap();
    register_payment_addresses(&mut state, [alice]);
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

    let transfer = verified_task(
        10,
        vec![Operation::Transfer {
            source: payment_address(alice),
            destination: payment_address(alice),
            amount: 1,
        }],
    );
    let repair = verified_task(
        11,
        vec![Operation::LeakRepair {
            leaked: vec![CurrencyAddress::new(2)],
        }],
    );
    let mut prepared = FinalityHarness::new("case");

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

#[test]
fn stale_book_cannot_cancel_task_after_another_book_begins_voting() {
    let alice = support::account(1);
    let store = StateStore::new(temp_base("stale-cancel-voting"));
    let set = validators();
    let mut state = SecondState::genesis([alice], 1);
    store.initialize(&state, &set).unwrap();

    let task = verified_task(
        900,
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );
    let mut original = PreparedTaskBook::new(store.clone()).unwrap();
    original.prepare(&mut state, &task, 1, &set).unwrap();

    let mut stale = PreparedTaskBook::new(store.clone()).unwrap();
    let mut voting = PreparedTaskBook::new(store.clone()).unwrap();
    voting
        .sign_prepared_vote(task.task_id(), ValidatorId::new(1), &key(4), &set)
        .unwrap();

    assert_eq!(
        stale.cancel(task.task_id()),
        Err(PreparationError::Persistence(
            PersistenceError::StalePreparedTasks
        ))
    );

    let mut recovered = PreparedTaskBook::new(store.clone()).unwrap();
    assert_eq!(
        recovered.cancel(task.task_id()),
        Err(PreparationError::CancellationClosed(task.task_id()))
    );

    store.remove_files().unwrap();
}

#[test]
fn stale_preparer_cannot_overwrite_newer_finalized_state() {
    let alice = support::account(1);
    let store = StateStore::new(temp_base("stale-finalized-state"));
    let set = validators();
    let initial = SecondState::genesis([alice], 1);
    store.initialize(&initial, &set).unwrap();

    let mut first_state = initial.clone();
    let mut stale_state = initial;
    let mut first = PreparedTaskBook::new(store.clone()).unwrap();
    let mut stale = PreparedTaskBook::new(store.clone()).unwrap();

    let first_task = verified_task(
        901,
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );
    first
        .prepare(&mut first_state, &first_task, 1, &set)
        .unwrap();
    let certificate = certify(&first, first_task.task_id(), &set);
    assert_eq!(
        first
            .commit(&mut first_state, first_task.task_id(), &certificate, &set)
            .unwrap(),
        ExecutionOutcome::Succeeded
    );

    let stale_task = verified_task(
        902,
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );
    assert_eq!(
        stale.prepare(&mut stale_state, &stale_task, 1, &set),
        Err(PreparationError::Persistence(PersistenceError::StaleState))
    );

    let restored = store.load().unwrap().unwrap();
    assert_eq!(restored.state.balance(alice), 1);
    assert_eq!(restored.state.next_currency_address(), 2);
    assert_eq!(stale_state.balance(alice), 0);
    assert_eq!(stale_state.next_currency_address(), 1);

    store.remove_files().unwrap();
}

#[test]
fn payment_address_lifecycle_claim_survives_restart_and_releases_on_cancel() {
    let alice = support::account(1);
    let address = support::payment(700);
    let store = StateStore::new(temp_base("payment-address-lifecycle-claim"));
    let validator_set = validators();
    let first = verified_task(
        930,
        vec![Operation::RegisterPaymentAddress {
            address,
            account: alice,
        }],
    );
    let second = verified_task(
        931,
        vec![Operation::RegisterPaymentAddress {
            address,
            account: alice,
        }],
    );

    let mut state = SecondState::genesis([alice], 1);
    {
        let mut prepared = PreparedTaskBook::new(store.clone()).unwrap();
        prepared
            .prepare(&mut state, &first, 1, &validator_set)
            .unwrap();
    }

    let restored = store.load().unwrap().unwrap();
    state = restored.state;
    let mut recovered = PreparedTaskBook::new(store.clone()).unwrap();

    assert_eq!(
        recovered.prepare(&mut state, &second, 2, &validator_set),
        Err(PreparationError::PaymentAddressContention(address))
    );

    recovered.cancel(first.task_id()).unwrap();
    assert_eq!(
        recovered
            .prepare(&mut state, &second, 2, &validator_set)
            .unwrap(),
        PreparationOutcome::Prepared
    );

    store.remove_files().unwrap();
}
