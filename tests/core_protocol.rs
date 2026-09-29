use second::{
    AccountAddress, CurrencyRole, ExecutionError, ExecutionOutcome, LegalTask, Operation,
    SecondState, TaskId,
};

fn digest(byte: u8) -> [u8; 32] {
    [byte; 32]
}

#[test]
fn issue_creates_distinct_currency_and_balance_is_derived() {
    let alice = AccountAddress::new(1);
    let mut state = SecondState::genesis([alice], 1000);

    let task = LegalTask::new(
        TaskId::new(1),
        digest(1),
        None,
        vec![Operation::Issue {
            account: alice,
            count: 3,
        }],
    );

    assert_eq!(
        state.execute(&task, 10).unwrap(),
        ExecutionOutcome::Succeeded
    );
    assert_eq!(state.balance(alice), 3);
    assert_eq!(state.current_supply(), 3);
    assert_eq!(state.next_currency_address(), 1003);
}

#[test]
fn transfer_selects_currency_dynamically_and_moves_exact_amount() {
    let alice = AccountAddress::new(1);
    let bob = AccountAddress::new(2);
    let mut state = SecondState::genesis([alice, bob], 1);

    let issue = LegalTask::new(
        TaskId::new(1),
        digest(1),
        None,
        vec![Operation::Issue {
            account: alice,
            count: 4,
        }],
    );
    state.execute(&issue, 1).unwrap();

    let transfer = LegalTask::new(
        TaskId::new(2),
        digest(2),
        None,
        vec![Operation::Transfer {
            source: alice,
            destination: bob,
            amount: 2,
        }],
    );
    state.execute(&transfer, 2).unwrap();

    assert_eq!(state.balance(alice), 2);
    assert_eq!(state.balance(bob), 2);

    assert_eq!(state.public_currency_states().len(), 4);
}

#[test]
fn public_currency_state_exposes_occupancy_but_not_owner() {
    let alice = AccountAddress::new(1);
    let mut state = SecondState::genesis([alice], 10);

    let task = LegalTask::new(
        TaskId::new(1),
        digest(1),
        None,
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );
    state.execute(&task, 1).unwrap();

    let public = state.public_currency_state(10.into()).unwrap();
    assert!(public.exists);
    assert!(public.occupied);
    assert_eq!(public.role, CurrencyRole::Circulation);

    let rendered = format!("{public:?}");
    assert!(!rendered.contains("AccountAddress"));
}

#[test]
fn failed_task_rolls_back_business_state_but_consumes_allocated_identity_range() {
    let alice = AccountAddress::new(1);
    let bob = AccountAddress::new(2);
    let mut state = SecondState::genesis([alice, bob], 500);

    let task = LegalTask::new(
        TaskId::new(77),
        digest(7),
        None,
        vec![
            Operation::Issue {
                account: alice,
                count: 2,
            },
            Operation::Transfer {
                source: bob,
                destination: alice,
                amount: 1,
            },
        ],
    );

    assert!(matches!(
        state.execute(&task, 10),
        Err(ExecutionError::InsufficientBalance { .. })
    ));

    assert_eq!(state.balance(alice), 0);
    assert_eq!(state.current_supply(), 0);
    assert_eq!(state.next_currency_address(), 502);
    assert_eq!(state.bound_request_digest(TaskId::new(77)), Some(digest(7)));
}

#[test]
fn task_id_binding_survives_failure_and_rejects_different_request() {
    let alice = AccountAddress::new(1);
    let bob = AccountAddress::new(2);
    let mut state = SecondState::genesis([alice, bob], 1);

    let first = LegalTask::new(
        TaskId::new(9),
        digest(1),
        None,
        vec![Operation::Transfer {
            source: alice,
            destination: bob,
            amount: 1,
        }],
    );
    assert!(state.execute(&first, 1).is_err());

    let different = LegalTask::new(
        TaskId::new(9),
        digest(2),
        None,
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );

    assert_eq!(
        state.execute(&different, 2),
        Err(ExecutionError::TaskIdAlreadyBound)
    );
}

#[test]
fn successful_task_replay_is_idempotent_even_after_expiry() {
    let alice = AccountAddress::new(1);
    let mut state = SecondState::genesis([alice], 1);

    let task = LegalTask::new(
        TaskId::new(1),
        digest(1),
        Some(5),
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );

    assert_eq!(
        state.execute(&task, 4).unwrap(),
        ExecutionOutcome::Succeeded
    );
    assert_eq!(
        state.execute(&task, 100).unwrap(),
        ExecutionOutcome::AlreadySucceeded
    );
    assert_eq!(state.balance(alice), 1);
    assert_eq!(state.current_supply(), 1);
}

#[test]
fn leak_repair_preserves_balance_supply_and_reserve_count() {
    let alice = AccountAddress::new(1);
    let mut state = SecondState::genesis([alice], 1).with_reserve(2).unwrap();

    let issue = LegalTask::new(
        TaskId::new(1),
        digest(1),
        None,
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );
    state.execute(&issue, 1).unwrap();

    let leaked = 3.into();
    let before_supply = state.current_supply();
    let before_reserve = state.reserve_count();

    let repair = LegalTask::new(
        TaskId::new(2),
        digest(2),
        None,
        vec![Operation::LeakRepair {
            leaked: vec![leaked],
        }],
    );
    state.execute(&repair, 2).unwrap();

    assert_eq!(state.balance(alice), 1);
    assert_eq!(state.current_supply(), before_supply);
    assert_eq!(state.reserve_count(), before_reserve);
    assert!(!state.currency_exists(leaked));
}
