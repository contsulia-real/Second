use ed25519_dalek::SigningKey;
use second::{
    AccountAddress, AuthorizerSet, CURRENT_PROTOCOL_VERSION, ClaimError, CurrencyClaimBook,
    LegalTask, LegalTaskPayload, Operation, OperationClaimId, SecondState, TaskId,
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

fn issued_state(count: u64) -> (SecondState, AccountAddress, AccountAddress) {
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
fn select_does_not_claim_until_claim_book_accepts_the_operation() {
    let (state, alice, _) = issued_state(3);
    let mut claims = CurrencyClaimBook::new();

    let first = claims
        .claim_transfer(&state, OperationClaimId::new(TaskId::new(10), 0), alice, 2)
        .unwrap();

    assert_eq!(first.len(), 2);
    assert_eq!(first[0].value(), 1);
    assert_eq!(first[1].value(), 2);

    let second = claims.claim_transfer(&state, OperationClaimId::new(TaskId::new(11), 0), alice, 2);

    assert_eq!(
        second,
        Err(ClaimError::CurrencyContention {
            account: alice,
            required: 2,
            available_unclaimed: 1,
            claimed_elsewhere: 2,
        })
    );
}

#[test]
fn true_insufficient_balance_is_not_reported_as_contention() {
    let (state, alice, _) = issued_state(1);
    let mut claims = CurrencyClaimBook::new();

    assert_eq!(
        claims.claim_transfer(&state, OperationClaimId::new(TaskId::new(10), 0), alice, 2,),
        Err(ClaimError::InsufficientBalance {
            account: alice,
            required: 2,
            available: 1,
        })
    );
}

#[test]
fn same_operation_claim_is_idempotent() {
    let (state, alice, _) = issued_state(3);
    let mut claims = CurrencyClaimBook::new();
    let id = OperationClaimId::new(TaskId::new(10), 4);

    let first = claims.claim_transfer(&state, id, alice, 2).unwrap();
    let second = claims.claim_transfer(&state, id, alice, 2).unwrap();

    assert_eq!(first, second);
    assert_eq!(claims.claimed_currency_count(), 2);
}

#[test]
fn same_claim_id_cannot_be_reused_for_different_transfer() {
    let (state, alice, bob) = issued_state(3);
    let mut claims = CurrencyClaimBook::new();
    let id = OperationClaimId::new(TaskId::new(10), 0);

    claims.claim_transfer(&state, id, alice, 1).unwrap();

    assert_eq!(
        claims.claim_transfer(&state, id, bob, 1),
        Err(ClaimError::ClaimIdentityConflict(id))
    );
}

#[test]
fn later_operation_in_same_task_can_reuse_a_task_claimed_currency() {
    let (state, alice, _) = issued_state(1);
    let mut claims = CurrencyClaimBook::new();
    let task_id = TaskId::new(10);
    let first_id = OperationClaimId::new(task_id, 0);
    let second_id = OperationClaimId::new(task_id, 1);

    let first = claims.claim_transfer(&state, first_id, alice, 1).unwrap();
    let second = claims.claim_transfer(&state, second_id, alice, 1).unwrap();

    assert_eq!(first, second);
    assert_eq!(claims.claimed_currency_count(), 1);

    claims.release(first_id);
    assert_eq!(claims.claimed_currency_count(), 1);

    claims.release(second_id);
    assert_eq!(claims.claimed_currency_count(), 0);
}

#[test]
fn releasing_one_operation_makes_its_currencies_claimable_again() {
    let (state, alice, _) = issued_state(2);
    let mut claims = CurrencyClaimBook::new();
    let first_id = OperationClaimId::new(TaskId::new(10), 0);

    claims.claim_transfer(&state, first_id, alice, 2).unwrap();
    claims.release(first_id);

    let claimed = claims
        .claim_transfer(&state, OperationClaimId::new(TaskId::new(11), 0), alice, 2)
        .unwrap();

    assert_eq!(claimed.len(), 2);
}

#[test]
fn releasing_task_releases_all_of_its_operation_claims() {
    let (state, alice, _) = issued_state(3);
    let mut claims = CurrencyClaimBook::new();
    let task_id = TaskId::new(10);

    claims
        .claim_transfer(&state, OperationClaimId::new(task_id, 0), alice, 1)
        .unwrap();
    claims
        .claim_transfer(&state, OperationClaimId::new(task_id, 1), alice, 2)
        .unwrap();

    assert_eq!(claims.claimed_currency_count(), 2);
    claims.release_task(task_id);
    assert_eq!(claims.claimed_currency_count(), 0);
}

#[test]
fn reserve_unavailable_and_reserve_contention_are_distinct() {
    let state = SecondState::genesis([], 1).with_reserve(2).unwrap();
    let mut claims = CurrencyClaimBook::new();

    claims
        .claim_reserve(OperationClaimId::new(TaskId::new(10), 0), &state, 2)
        .unwrap();

    assert_eq!(
        claims.claim_reserve(OperationClaimId::new(TaskId::new(11), 0), &state, 1),
        Err(ClaimError::ReserveContention {
            required: 1,
            available_unclaimed: 0,
            claimed_elsewhere: 2,
        })
    );

    let mut empty_claims = CurrencyClaimBook::new();
    assert_eq!(
        empty_claims.claim_reserve(OperationClaimId::new(TaskId::new(12), 0), &state, 3),
        Err(ClaimError::ReserveUnavailable {
            required: 3,
            available: 2,
        })
    );
}
