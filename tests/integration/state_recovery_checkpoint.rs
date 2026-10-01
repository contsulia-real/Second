use crate::support::{self, certificate_from_keys, key, temp_base, validator_set, verified_task};
use second::{
    CertifiedStateRecoveryCheckpoint, Operation, PreparedTaskBook, SecondState,
    StateRecoveryCheckpoint, StateStore, ValidatorId, ValidatorSigner, ValidatorSigningError,
};

#[test]
fn shared_recovery_checkpoint_ignores_validator_local_vote_locks_and_forms_qc() {
    let validators = validator_set(7, 1..=4);
    let state = SecondState::genesis([], 1).with_reserve(2).unwrap();

    let clean_store = StateStore::new(temp_base("recovery-shared-clean"));
    let locked_store = StateStore::new(temp_base("recovery-shared-locked"));
    clean_store.initialize(&state, &validators).unwrap();
    locked_store.initialize(&state, &validators).unwrap();

    let clean =
        StateRecoveryCheckpoint::from_persisted(11, &clean_store.load().unwrap().unwrap()).unwrap();

    ValidatorSigner::new(ValidatorId::new(1), key(4), locked_store.clone())
        .sign_state_recovery_checkpoint(&clean, &validators)
        .unwrap();

    let locked =
        StateRecoveryCheckpoint::from_persisted(11, &locked_store.load().unwrap().unwrap())
            .unwrap();
    assert_eq!(clean, locked);

    let statement = clean.finality_statement();
    let votes = [1_u64, 2, 3]
        .into_iter()
        .map(|id| support::signed_vote(&statement, ValidatorId::new(id), &key((id * 3 + 1) as u8)))
        .collect();
    let certified =
        CertifiedStateRecoveryCheckpoint::new(clean.clone(), votes, &validators).unwrap();

    assert_eq!(certified.checkpoint(), &clean);
    assert_eq!(certified.certificate().vote_count(), 3);

    clean_store.remove_files().unwrap();
    locked_store.remove_files().unwrap();
}

#[test]
fn recovery_checkpoint_vote_lock_blocks_same_serial_after_committed_state_changes() {
    let validators = validator_set(7, 1..=4);
    let store = StateStore::new(temp_base("recovery-serial-lock"));
    let mut state = SecondState::genesis([support::account(1)], 1);
    store.initialize(&state, &validators).unwrap();

    let signer = ValidatorSigner::new(ValidatorId::new(1), key(4), store.clone());
    let first =
        StateRecoveryCheckpoint::from_persisted(20, &store.load().unwrap().unwrap()).unwrap();
    signer
        .sign_state_recovery_checkpoint(&first, &validators)
        .unwrap();

    let task = verified_task(
        500,
        vec![Operation::Issue {
            account: support::account(1),
            count: 1,
        }],
    );
    let mut book = PreparedTaskBook::new(store.clone()).unwrap();
    book.prepare(&mut state, &task, 0, &validators).unwrap();
    let statement = book.prepared_finality_statement(task.task_id()).unwrap();
    let certificate = certificate_from_keys(
        statement,
        &validators,
        [1_u64, 2, 3]
            .into_iter()
            .map(|id| (ValidatorId::new(id), key((id * 3 + 1) as u8))),
    );
    book.commit(&mut state, task.task_id(), &certificate)
        .unwrap();

    let conflicting =
        StateRecoveryCheckpoint::from_persisted(20, &store.load().unwrap().unwrap()).unwrap();
    assert_ne!(
        first.shared_state_digest(),
        conflicting.shared_state_digest()
    );

    assert!(matches!(
        signer.sign_state_recovery_checkpoint(&conflicting, &validators),
        Err(ValidatorSigningError::VoteLocked {
            validator_id,
            ..
        }) if validator_id == ValidatorId::new(1)
    ));

    store.remove_files().unwrap();
}
