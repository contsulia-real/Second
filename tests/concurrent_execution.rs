use ed25519_dalek::SigningKey;
use second::{
    AccountAddress, AuthorizerSet, CURRENT_PROTOCOL_VERSION, ClaimError, ConcurrentExecutionError,
    CurrencyClaimBook, ExecutionOutcome, LegalTask, LegalTaskPayload, Operation, OperationClaimId,
    SecondState, TaskId,
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

fn state_with_balance(count: u64) -> (SecondState, AccountAddress, AccountAddress) {
    let alice = AccountAddress::new(1);
    let bob = AccountAddress::new(2);
    let mut state = SecondState::genesis([alice, bob], 1);
    state
        .execute(
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
    let mut claims = CurrencyClaimBook::new();

    claims
        .claim_transfer(&state, OperationClaimId::new(TaskId::new(100), 0), alice, 2)
        .unwrap();

    let competing = verified_task(
        200,
        vec![Operation::Transfer {
            source: alice,
            destination: bob,
            amount: 2,
        }],
    );

    assert_eq!(
        state.execute_with_claims(&competing, 2, &mut claims),
        Err(ConcurrentExecutionError::Claim(
            ClaimError::CurrencyContention {
                account: alice,
                required: 2,
                available_unclaimed: 1,
                claimed_elsewhere: 2,
            }
        ))
    );

    assert_eq!(state.balance(alice), 3);
    assert_eq!(state.balance(bob), 0);
}

#[test]
fn transfer_succeeds_after_competing_task_releases_claims() {
    let (mut state, alice, bob) = state_with_balance(2);
    let mut claims = CurrencyClaimBook::new();
    let blocker = OperationClaimId::new(TaskId::new(100), 0);

    claims.claim_transfer(&state, blocker, alice, 2).unwrap();
    claims.release(blocker);

    let task = verified_task(
        200,
        vec![Operation::Transfer {
            source: alice,
            destination: bob,
            amount: 2,
        }],
    );

    assert_eq!(
        state.execute_with_claims(&task, 2, &mut claims).unwrap(),
        ExecutionOutcome::Succeeded
    );
    assert_eq!(state.balance(alice), 0);
    assert_eq!(state.balance(bob), 2);
    assert_eq!(claims.claimed_currency_count(), 0);
}

#[test]
fn ordered_operations_in_one_task_can_reuse_the_same_claimed_currency() {
    let alice = AccountAddress::new(1);
    let bob = AccountAddress::new(2);
    let charlie = AccountAddress::new(3);
    let mut state = SecondState::genesis([alice, bob, charlie], 1);
    let mut claims = CurrencyClaimBook::new();

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

    let task = verified_task(
        2,
        vec![
            Operation::Transfer {
                source: alice,
                destination: bob,
                amount: 1,
            },
            Operation::Transfer {
                source: bob,
                destination: charlie,
                amount: 1,
            },
        ],
    );

    assert_eq!(
        state.execute_with_claims(&task, 2, &mut claims).unwrap(),
        ExecutionOutcome::Succeeded
    );
    assert_eq!(state.balance(alice), 0);
    assert_eq!(state.balance(bob), 0);
    assert_eq!(state.balance(charlie), 1);
    assert_eq!(claims.claimed_currency_count(), 0);
}

#[test]
fn leak_repair_reports_reserve_contention_when_reserve_is_claimed_elsewhere() {
    let alice = AccountAddress::new(1);
    let mut state = SecondState::genesis([alice], 1).with_reserve(1).unwrap();
    let mut claims = CurrencyClaimBook::new();

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

    claims
        .claim_reserve(OperationClaimId::new(TaskId::new(100), 0), &state, 1)
        .unwrap();

    let repair = verified_task(
        200,
        vec![Operation::LeakRepair {
            leaked: vec![2.into()],
        }],
    );

    assert_eq!(
        state.execute_with_claims(&repair, 2, &mut claims),
        Err(ConcurrentExecutionError::Claim(
            ClaimError::ReserveContention {
                required: 1,
                available_unclaimed: 0,
                claimed_elsewhere: 1,
            }
        ))
    );

    assert!(state.currency_exists(2.into()));
    assert_eq!(state.reserve_count(), 1);
}

#[test]
fn failed_claimed_execution_releases_its_earlier_operation_claims() {
    let (mut state, alice, bob) = state_with_balance(1);
    let charlie = AccountAddress::new(3);
    let mut claims = CurrencyClaimBook::new();

    let task = verified_task(
        300,
        vec![
            Operation::Transfer {
                source: alice,
                destination: bob,
                amount: 1,
            },
            Operation::Transfer {
                source: charlie,
                destination: bob,
                amount: 1,
            },
        ],
    );

    assert!(matches!(
        state.execute_with_claims(&task, 2, &mut claims),
        Err(ConcurrentExecutionError::Execution(_))
    ));
    assert_eq!(claims.claimed_currency_count(), 0);
    assert_eq!(state.balance(alice), 1);
    assert_eq!(state.balance(bob), 0);
}
