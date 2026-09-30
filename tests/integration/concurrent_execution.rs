use crate::support;
use support::FinalizedExecute as _;

use second::{
    AccountAddress, ClaimError, ExecutionOutcome, Operation, PreparationError, PreparationOutcome,
    SecondState,
};
use support::{FinalityHarness, payment_address, register_payment_addresses, verified_task};

fn state_with_balance(count: u64) -> (SecondState, AccountAddress, AccountAddress) {
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
                    count,
                }],
            ),
            1,
        )
        .unwrap();
    (state, alice, bob)
}

#[test]
fn in_flight_claim_turns_sufficient_balance_into_currency_contention() {
    let (mut state, alice, bob) = state_with_balance(3);
    let mut finalized = FinalityHarness::new("currency-contention");

    let blocker = verified_task(
        100,
        vec![Operation::Transfer {
            source: payment_address(alice),
            destination: payment_address(bob),
            amount: 2,
        }],
    );
    assert_eq!(
        finalized.prepare(&mut state, &blocker, 2).unwrap(),
        PreparationOutcome::Prepared
    );

    let competing = verified_task(
        200,
        vec![Operation::Transfer {
            source: payment_address(alice),
            destination: payment_address(bob),
            amount: 2,
        }],
    );
    assert_eq!(
        finalized.prepare(&mut state, &competing, 2),
        Err(PreparationError::Claim(ClaimError::CurrencyContention {
            account: alice,
            required: 2,
            available_unclaimed: 1,
            claimed_elsewhere: 2,
        }))
    );

    assert_eq!(state.balance(alice), 3);
    assert_eq!(state.balance(bob), 0);
}

#[test]
fn transfer_succeeds_after_competing_task_releases_claims() {
    let (mut state, alice, bob) = state_with_balance(2);
    let mut finalized = FinalityHarness::new("released-claims");

    let blocker = verified_task(
        100,
        vec![Operation::Transfer {
            source: payment_address(alice),
            destination: payment_address(bob),
            amount: 2,
        }],
    );
    assert_eq!(
        finalized.prepare(&mut state, &blocker, 2).unwrap(),
        PreparationOutcome::Prepared
    );
    finalized.cancel(blocker.task_id()).unwrap();

    let task = verified_task(
        200,
        vec![Operation::Transfer {
            source: payment_address(alice),
            destination: payment_address(bob),
            amount: 2,
        }],
    );

    assert_eq!(
        finalized.execute(&mut state, &task, 2).unwrap(),
        ExecutionOutcome::Succeeded
    );
    assert_eq!(state.balance(alice), 0);
    assert_eq!(state.balance(bob), 2);
    assert_eq!(finalized.claimed_currency_count(), 0);
}

#[test]
fn ordered_operations_in_one_task_can_reuse_the_same_claimed_currency() {
    let alice = support::account(1);
    let bob = support::account(2);
    let charlie = support::account(3);
    let mut state = SecondState::genesis([alice, bob, charlie], 1);
    register_payment_addresses(&mut state, [alice, bob, charlie]);
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

    let task = verified_task(
        2,
        vec![
            Operation::Transfer {
                source: payment_address(alice),
                destination: payment_address(bob),
                amount: 1,
            },
            Operation::Transfer {
                source: payment_address(bob),
                destination: payment_address(charlie),
                amount: 1,
            },
        ],
    );
    let mut finalized = FinalityHarness::new("same-task-claim-reuse");

    assert_eq!(
        finalized.execute(&mut state, &task, 2).unwrap(),
        ExecutionOutcome::Succeeded
    );
    assert_eq!(state.balance(alice), 0);
    assert_eq!(state.balance(bob), 0);
    assert_eq!(state.balance(charlie), 1);
    assert_eq!(finalized.claimed_currency_count(), 0);
}

#[test]
fn leak_repair_validates_leaked_currency_before_reserve_availability() {
    let mut state = SecondState::genesis([], 1);
    let task = verified_task(
        199,
        vec![Operation::LeakRepair {
            leaked: vec![999.into()],
        }],
    );
    let mut finalized = FinalityHarness::new("leak-validation");

    assert_eq!(
        finalized.prepare(&mut state, &task, 2),
        Err(PreparationError::Execution(
            second::ExecutionError::CurrencyNotFound(999.into())
        ))
    );
    assert_eq!(finalized.claimed_currency_count(), 0);
}

#[test]
fn leak_repair_reports_reserve_contention_when_reserve_is_claimed_elsewhere() {
    let alice = support::account(1);
    let mut state = SecondState::genesis([alice], 1).with_reserve(1).unwrap();
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

    let mut finalized = FinalityHarness::new("reserve-contention");
    let blocker = verified_task(
        100,
        vec![Operation::LeakRepair {
            leaked: vec![2.into()],
        }],
    );
    assert_eq!(
        finalized.prepare(&mut state, &blocker, 2).unwrap(),
        PreparationOutcome::Prepared
    );

    let repair = verified_task(
        200,
        vec![Operation::LeakRepair {
            leaked: vec![3.into()],
        }],
    );
    assert_eq!(
        finalized.prepare(&mut state, &repair, 2),
        Err(PreparationError::Claim(ClaimError::ReserveContention {
            required: 1,
            available_unclaimed: 0,
            claimed_elsewhere: 1,
        }))
    );

    assert!(state.currency_exists(2.into()));
    assert!(state.currency_exists(3.into()));
    assert_eq!(state.reserve_count(), 1);
}

#[test]
fn failed_prepare_releases_its_earlier_operation_claims() {
    let (mut state, alice, bob) = state_with_balance(1);
    let charlie = support::account(3);
    let mut finalized = FinalityHarness::new("failed-prepare-claims");

    let task = verified_task(
        300,
        vec![
            Operation::Transfer {
                source: payment_address(alice),
                destination: payment_address(bob),
                amount: 1,
            },
            Operation::Transfer {
                source: payment_address(charlie),
                destination: payment_address(bob),
                amount: 1,
            },
        ],
    );

    assert!(matches!(
        finalized.prepare(&mut state, &task, 2),
        Err(PreparationError::Execution(_))
    ));
    assert_eq!(finalized.claimed_currency_count(), 0);
    assert_eq!(state.balance(alice), 1);
    assert_eq!(state.balance(bob), 0);
}
