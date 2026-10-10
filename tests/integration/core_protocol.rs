use crate::support;
use support::FinalizedExecute as _;

use second::{
    AuthorizerSet, CURRENT_PROTOCOL_VERSION, ExecutionError, ExecutionOutcome, LegalTaskPayload,
    Operation, SecondState, VerifiedLegalTask,
};
use support::{key as test_key, payment_address, register_payment_addresses};

fn verified_task(
    task_id: u128,
    expires_at: Option<u64>,
    operations: Vec<Operation>,
) -> VerifiedLegalTask {
    let key = test_key(7);
    let authorizers =
        AuthorizerSet::new(CURRENT_PROTOCOL_VERSION, [key.verifying_key().to_bytes()]).unwrap();
    let payload = LegalTaskPayload::new(
        support::task_id(task_id),
        CURRENT_PROTOCOL_VERSION,
        expires_at,
        operations,
    );

    support::sign_task(payload, &key)
        .unwrap()
        .verify(&authorizers)
        .unwrap()
}

#[test]
fn issue_creates_distinct_currency_and_balance_is_derived() {
    let alice = support::account(1);
    let mut state = SecondState::genesis([alice], 1000);

    let task = verified_task(
        1,
        None,
        vec![Operation::Issue {
            account: alice,
            count: 3,
        }],
    );

    assert_eq!(
        state.execute_finalized(&task, 10).unwrap(),
        ExecutionOutcome::Succeeded
    );
    assert_eq!(state.balance(alice), 3);
    assert_eq!(state.current_supply(), 3);
    assert_eq!(state.next_currency_address(), 1003);
}

#[test]
fn transfer_selects_currency_dynamically_and_moves_exact_amount() {
    let alice = support::account(1);
    let bob = support::account(2);
    let mut state = SecondState::genesis([alice, bob], 1);
    register_payment_addresses(&mut state, [alice, bob]);

    let issue = verified_task(
        1,
        None,
        vec![Operation::Issue {
            account: alice,
            count: 4,
        }],
    );
    state.execute_finalized(&issue, 1).unwrap();

    let transfer = verified_task(
        2,
        None,
        vec![Operation::Transfer {
            source: payment_address(alice),
            destination: payment_address(bob),
            amount: 2,
        }],
    );
    state.execute_finalized(&transfer, 2).unwrap();

    assert_eq!(state.balance(alice), 2);
    assert_eq!(state.balance(bob), 2);
    assert_eq!(state.public_currency_states().len(), 1);
    assert_eq!(state.public_currency_states()[0].len, 4);
}

#[test]
fn public_currency_state_exposes_occupancy_but_not_owner() {
    let alice = support::account(1);
    let mut state = SecondState::genesis([alice], 10);

    let task = verified_task(
        1,
        None,
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );
    state.execute_finalized(&task, 1).unwrap();

    let public = state.public_currency_state(10.into()).unwrap();
    assert!(public.occupied);
    assert_eq!(public.len, 1);

    let rendered = format!("{public:?}");
    assert!(!rendered.contains("AccountAddress"));
}

#[test]
fn failed_task_rolls_back_business_state_but_consumes_allocated_identity_range() {
    let alice = support::account(1);
    let bob = support::account(2);
    let mut state = SecondState::genesis([alice, bob], 500);
    register_payment_addresses(&mut state, [alice, bob]);

    let task = verified_task(
        77,
        None,
        vec![
            Operation::Issue {
                account: alice,
                count: 2,
            },
            Operation::Transfer {
                source: payment_address(bob),
                destination: payment_address(alice),
                amount: 1,
            },
        ],
    );
    let request_digest = task.request_digest();

    assert!(matches!(
        state.execute_finalized(&task, 10),
        Err(ExecutionError::InsufficientBalance { .. })
    ));

    assert_eq!(state.balance(alice), 0);
    assert_eq!(state.current_supply(), 0);
    assert_eq!(state.next_currency_address(), 502);
    assert_eq!(
        state.bound_request_digest(support::task_id(77)),
        Some(request_digest)
    );
}

#[test]
fn task_id_binding_survives_failure_and_rejects_different_request() {
    let alice = support::account(1);
    let bob = support::account(2);
    let mut state = SecondState::genesis([alice, bob], 1);
    register_payment_addresses(&mut state, [alice, bob]);

    let first = verified_task(
        9,
        None,
        vec![Operation::Transfer {
            source: payment_address(alice),
            destination: payment_address(bob),
            amount: 1,
        }],
    );
    assert!(state.execute_finalized(&first, 1).is_err());

    let different = verified_task(
        9,
        None,
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );

    assert_eq!(
        state.execute_finalized(&different, 2),
        Err(ExecutionError::TaskIdAlreadyBound)
    );
}

#[test]
fn successful_task_replay_is_idempotent_even_after_expiry() {
    let alice = support::account(1);
    let mut state = SecondState::genesis([alice], 1);

    let task = verified_task(
        1,
        Some(5),
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );

    assert_eq!(
        state.execute_finalized(&task, 4).unwrap(),
        ExecutionOutcome::Succeeded
    );
    assert_eq!(
        state.execute_finalized(&task, 100).unwrap(),
        ExecutionOutcome::AlreadySucceeded
    );
    assert_eq!(state.balance(alice), 1);
    assert_eq!(state.current_supply(), 1);
}

#[test]
fn leak_repair_preserves_balance_supply_and_reserve_count() {
    let alice = support::account(1);
    let mut state = SecondState::genesis([alice], 1).with_reserve(2).unwrap();

    let issue = verified_task(
        1,
        None,
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );
    state.execute_finalized(&issue, 1).unwrap();

    let leaked = 3.into();
    let before_supply = state.current_supply();
    let before_reserve = state.reserve_count();

    let repair = verified_task(
        2,
        None,
        vec![Operation::LeakRepair {
            leaked: vec![leaked],
        }],
    );
    state.execute_finalized(&repair, 2).unwrap();

    assert_eq!(state.balance(alice), 1);
    assert_eq!(state.current_supply(), before_supply);
    assert_eq!(state.reserve_count(), before_reserve);
    assert!(!state.currency_exists(leaked));
}

#[test]
fn currency_sequence_allocator_reports_numeric_exhaustion() {
    let alice = support::account(1);
    let mut state = SecondState::genesis([alice], u64::MAX - 1);

    let last = verified_task(
        900,
        None,
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );
    assert_eq!(
        state.execute_finalized(&last, 1).unwrap(),
        ExecutionOutcome::Succeeded
    );
    assert!(state.currency_exists(second::CurrencyAddress::new(u64::MAX - 1)));

    let exhausted = verified_task(
        901,
        None,
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );
    assert_eq!(
        state.execute_finalized(&exhausted, 2),
        Err(ExecutionError::CurrencySequenceSpaceExhausted)
    );
}
