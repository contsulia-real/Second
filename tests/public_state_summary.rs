mod support;

use second::{Operation, SecondState};
use support::{payment_address, register_payment_addresses, verified_task};

#[test]
fn public_summary_is_identical_when_only_hidden_owner_differs() {
    let alice = support::account(1);
    let bob = support::account(2);

    let mut left = SecondState::genesis([alice, bob], 1);
    left.execute(
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

    let mut right = SecondState::genesis([alice, bob], 1);
    right
        .execute(
            &verified_task(
                2,
                vec![Operation::Issue {
                    account: bob,
                    count: 1,
                }],
            ),
            1,
        )
        .unwrap();

    let left_summary = left.public_currency_summary();
    let right_summary = right.public_currency_summary();

    assert_eq!(left_summary, right_summary);
    assert_eq!(left_summary.current_supply, 1);
    assert_eq!(left_summary.occupied_count, 1);

    let rendered = format!("{left_summary:?}");
    assert!(!rendered.contains("AccountAddress"));
}

#[test]
fn transfer_between_owners_does_not_change_public_currency_digest() {
    let alice = support::account(1);
    let bob = support::account(2);
    let mut state = SecondState::genesis([alice, bob], 1);
    register_payment_addresses(&mut state, [alice, bob]);

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

    let before = state.public_currency_summary();

    state
        .execute(
            &verified_task(
                2,
                vec![Operation::Transfer {
                    source: payment_address(alice),
                    destination: payment_address(bob),
                    amount: 1,
                }],
            ),
            2,
        )
        .unwrap();

    let after = state.public_currency_summary();

    assert_eq!(before, after);
}

#[test]
fn public_summary_changes_when_occupancy_or_role_changes() {
    let alice = support::account(1);

    let reserve_only = SecondState::genesis([alice], 1).with_reserve(1).unwrap();

    let mut circulation = SecondState::genesis([alice], 1);
    circulation
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

    let reserve_summary = reserve_only.public_currency_summary();
    let circulation_summary = circulation.public_currency_summary();

    assert_ne!(
        reserve_summary.state_digest,
        circulation_summary.state_digest
    );
    assert_eq!(reserve_summary.reserve_count, 1);
    assert_eq!(reserve_summary.occupied_count, 0);
    assert_eq!(circulation_summary.reserve_count, 0);
    assert_eq!(circulation_summary.occupied_count, 1);
}
