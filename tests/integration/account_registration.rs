use second::{
    ExecutionError, Operation, PreparationError, PreparedTaskBook, SecondState, StateStore,
};

use crate::support::{self, certificate_from_keys, key, temp_base, validator_set, verified_task};

#[test]
fn account_registration_claim_survives_restart_and_releases_on_cancel() {
    let base = temp_base("account-registration-claim");
    let store = StateStore::new(&base);
    let validators = validator_set(1, 1..=4);
    let account = support::account(601);
    store
        .initialize(&SecondState::genesis([], 1), &validators)
        .unwrap();
    let mut state = store.load().unwrap().unwrap().state;
    let a = verified_task(6001, vec![Operation::RegisterAccount { account }]);
    let b = verified_task(6002, vec![Operation::RegisterAccount { account }]);
    let mut book = PreparedTaskBook::new(store.clone()).unwrap();
    book.prepare(&mut state, &a, 1, &validators).unwrap();
    assert!(!state.has_account(account));
    drop(book);
    let reopened = StateStore::new(&base);
    state = reopened.load().unwrap().unwrap().state;
    assert!(!state.has_account(account));
    let mut book = PreparedTaskBook::new(reopened.clone()).unwrap();
    assert_eq!(
        book.prepare(&mut state, &b, 1, &validators),
        Err(PreparationError::AccountContention(account))
    );
    book.cancel(a.task_id()).unwrap();
    book.prepare(&mut state, &b, 1, &validators).unwrap();
    let statement = book.prepared_finality_statement(b.task_id()).unwrap();
    let certificate = certificate_from_keys(
        statement,
        &validators,
        (1..=3).map(|id| (second::ValidatorId::new(id), key((id * 3 + 1) as u8))),
    );
    book.commit(&mut state, b.task_id(), &certificate).unwrap();
    assert!(
        StateStore::new(&base)
            .load()
            .unwrap()
            .unwrap()
            .state
            .has_account(account)
    );
    let duplicate = verified_task(6003, vec![Operation::RegisterAccount { account }]);
    assert_eq!(
        book.prepare(&mut state, &duplicate, 1, &validators),
        Err(PreparationError::Execution(
            ExecutionError::AccountAlreadyExists(account)
        ))
    );
    drop(book);
    reopened.remove_files().unwrap();
}

#[test]
fn duplicate_registration_in_one_task_rolls_back_creation_and_releases_claim() {
    let mut state = SecondState::genesis([], 1);
    let account = support::account(602);
    let mut harness = support::FinalityHarness::new("account-registration-rollback");
    let bad = verified_task(
        6010,
        vec![
            Operation::RegisterAccount { account },
            Operation::RegisterAccount { account },
        ],
    );
    assert_eq!(
        harness.prepare(&mut state, &bad, 1),
        Err(PreparationError::Execution(
            ExecutionError::AccountAlreadyExists(account)
        ))
    );
    assert!(!state.has_account(account));
    let good = verified_task(6011, vec![Operation::RegisterAccount { account }]);
    harness.prepare(&mut state, &good, 1).unwrap();
    harness.commit(&mut state, good.task_id()).unwrap();
    assert!(state.has_account(account));
}
