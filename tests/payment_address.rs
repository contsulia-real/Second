mod support;

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
        state.execute(&established, 1),
        Err(ExecutionError::InsufficientBalance {
            account: alice,
            required: 1,
            available: 0,
        })
    );
    assert_eq!(state.payment_execution_count(), 1);

    state
        .execute(
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
        state.execute(&new_transfer, 3),
        Err(ExecutionError::PaymentAddressUnavailable(alice_pay))
    );

    state.execute(&established, 10).unwrap();

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
fn expired_transfer_reservations_are_reaped_in_expiry_order() {
    let a = support::account(1);
    let b = support::account(2);
    let c = support::account(3);
    let d = support::account(4);
    let a_pay = support::payment(101);
    let b_pay = support::payment(102);
    let c_pay = support::payment(103);
    let d_pay = support::payment(104);
    let mut state = SecondState::genesis([a, b, c, d], 1);

    for (address, account) in [(a_pay, a), (b_pay, b), (c_pay, c), (d_pay, d)] {
        state.register_payment_address(address, account).unwrap();
    }

    let later = verified_task_with_expiry(
        1,
        Some(20),
        vec![Operation::Transfer {
            source: a_pay,
            destination: b_pay,
            amount: 1,
        }],
    );
    let earlier = verified_task_with_expiry(
        2,
        Some(10),
        vec![Operation::Transfer {
            source: c_pay,
            destination: d_pay,
            amount: 1,
        }],
    );

    assert!(matches!(
        state.execute(&later, 1),
        Err(ExecutionError::InsufficientBalance { .. })
    ));
    assert!(matches!(
        state.execute(&earlier, 1),
        Err(ExecutionError::InsufficientBalance { .. })
    ));
    assert_eq!(state.payment_execution_count(), 2);

    assert_eq!(state.reap_expired_payment_executions(15, 1), 1);
    assert_eq!(state.payment_execution_count(), 1);
    assert_eq!(state.reap_expired_payment_executions(15, 1), 0);

    assert_eq!(state.reap_expired_payment_executions(25, 1), 1);
    assert_eq!(state.payment_execution_count(), 0);
}
