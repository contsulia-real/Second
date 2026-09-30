use crate::support;
use support::FinalizedExecute as _;

use second::{ExecutionError, Operation, PaymentAddressStatus, SecondState};
use support::verified_task_with_expiry;

#[test]
fn retiring_blocks_new_transfer_but_established_transfer_can_finish() {
    let alice = support::account(1);
    let bob = support::account(2);
    let alice_pay = support::payment(101);
    let bob_pay = support::payment(102);
    let mut state = SecondState::genesis([alice, bob], 1);

    state.register_payment_address(alice_pay, alice).unwrap();
    state.register_payment_address(bob_pay, bob).unwrap();

    let established = verified_task_with_expiry(
        10,
        Some(100),
        vec![Operation::Transfer {
            source: alice_pay,
            destination: bob_pay,
            amount: 1,
        }],
    );

    assert_eq!(
        state.execute_finalized(&established, 1),
        Err(ExecutionError::InsufficientBalance {
            account: alice,
            required: 1,
            available: 0,
        })
    );
    assert_eq!(state.payment_execution_count(), 1);

    state
        .execute_finalized(
            &verified_task_with_expiry(
                11,
                None,
                vec![Operation::Issue {
                    account: alice,
                    count: 1,
                }],
            ),
            2,
        )
        .unwrap();

    state.retire_payment_address(alice_pay).unwrap();
    state.retire_payment_address(bob_pay).unwrap();
    assert_eq!(
        state.payment_address_status(alice_pay),
        Some(PaymentAddressStatus::Retiring)
    );
    assert_eq!(
        state.finalize_payment_address_retirement(alice_pay),
        Err(ExecutionError::InvalidPaymentAddressTransition(alice_pay))
    );
    assert_eq!(
        state.finalize_payment_address_retirement(bob_pay),
        Err(ExecutionError::InvalidPaymentAddressTransition(bob_pay))
    );

    let new_transfer = verified_task_with_expiry(
        12,
        Some(100),
        vec![Operation::Transfer {
            source: alice_pay,
            destination: bob_pay,
            amount: 1,
        }],
    );
    assert_eq!(
        state.execute_finalized(&new_transfer, 3),
        Err(ExecutionError::PaymentAddressUnavailable(alice_pay))
    );

    state.execute_finalized(&established, 10).unwrap();

    assert_eq!(state.balance(alice), 0);
    assert_eq!(state.balance(bob), 1);
    assert_eq!(state.payment_execution_count(), 0);

    state
        .finalize_payment_address_retirement(alice_pay)
        .unwrap();
    state.finalize_payment_address_retirement(bob_pay).unwrap();
    assert_eq!(
        state.payment_address_status(alice_pay),
        Some(PaymentAddressStatus::Retired)
    );
    assert_eq!(
        state.payment_address_status(bob_pay),
        Some(PaymentAddressStatus::Retired)
    );
}

#[test]
fn later_transfer_is_not_established_when_an_earlier_operation_fails() {
    let alice = support::account(1);
    let bob = support::account(2);
    let missing = support::account(99);
    let alice_pay = support::payment(101);
    let bob_pay = support::payment(102);
    let mut state = SecondState::genesis([alice, bob], 1);

    state.register_payment_address(alice_pay, alice).unwrap();
    state.register_payment_address(bob_pay, bob).unwrap();

    let task = verified_task_with_expiry(
        20,
        Some(100),
        vec![
            Operation::Issue {
                account: missing,
                count: 1,
            },
            Operation::Transfer {
                source: alice_pay,
                destination: bob_pay,
                amount: 1,
            },
        ],
    );

    assert_eq!(
        state.execute_finalized(&task, 1),
        Err(ExecutionError::AccountNotFound(missing))
    );
    assert_eq!(state.payment_execution_count(), 0);
}
