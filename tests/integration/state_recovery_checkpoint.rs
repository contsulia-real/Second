use crate::support::{self, certificate_from_keys, key, temp_base, validator_set, verified_task};
use second::{
    CURRENT_PROTOCOL_VERSION, CertifiedStateRecoveryCheckpoint, CertifiedValidatorSetTransition,
    ConsensusScope, Operation, PreparedTaskBook, SecondState, StateRecoveryCheckpoint, StateStore,
    ValidatorId, ValidatorSet, ValidatorSetTransition, ValidatorSigner, ValidatorSigningError,
};

fn mark_recovery_finality_ready(
    store: &StateStore,
    validator_id: ValidatorId,
    checkpoint: &StateRecoveryCheckpoint,
    validators: &ValidatorSet,
) {
    support::mark_bft_finality_ready(
        store,
        validator_id,
        ConsensusScope::StateRecoveryCheckpoint {
            validator_set_version: checkpoint.validator_set_version(),
            serial: checkpoint.serial(),
        },
        checkpoint.digest(),
        validators,
        [1_u64, 2, 3]
            .into_iter()
            .map(|id| (ValidatorId::new(id), key((id * 3 + 1) as u8))),
    );
}

#[test]
fn shared_recovery_checkpoint_ignores_validator_local_vote_locks_and_forms_qc() {
    let validators = validator_set(7, 1..=4);
    let state = SecondState::genesis([], 1).with_reserve(2).unwrap();

    let clean_store = StateStore::new(temp_base("recovery-shared-clean"));
    let locked_store = StateStore::new(temp_base("recovery-shared-locked"));
    clean_store.initialize(&state, &validators).unwrap();
    locked_store.initialize(&state, &validators).unwrap();

    let clean =
        StateRecoveryCheckpoint::from_persisted(1, &clean_store.load().unwrap().unwrap()).unwrap();

    mark_recovery_finality_ready(&locked_store, ValidatorId::new(1), &clean, &validators);
    ValidatorSigner::new(ValidatorId::new(1), key(4), locked_store.clone())
        .sign_state_recovery_checkpoint(&clean, &validators)
        .unwrap();

    let locked =
        StateRecoveryCheckpoint::from_persisted(1, &locked_store.load().unwrap().unwrap()).unwrap();
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
fn recovery_serial_requires_certified_previous_checkpoint() {
    let validators = validator_set(7, 1..=4);
    let store = StateStore::new(temp_base("recovery-serial-sequence"));
    let state = SecondState::genesis([], 1).with_reserve(1).unwrap();
    store.initialize(&state, &validators).unwrap();
    let signer = ValidatorSigner::new(ValidatorId::new(1), key(4), store.clone());

    let skipped_first =
        StateRecoveryCheckpoint::from_persisted(2, &store.load().unwrap().unwrap()).unwrap();
    assert_eq!(
        signer.sign_state_recovery_checkpoint(&skipped_first, &validators),
        Err(ValidatorSigningError::Persistence(
            second::PersistenceError::UnexpectedRecoveryCheckpointSerial {
                validator_set_version: 7,
                expected: 1,
                actual: 2,
            }
        ))
    );

    let first = store.next_state_recovery_checkpoint().unwrap();
    assert_eq!(first.serial(), 1);
    mark_recovery_finality_ready(&store, ValidatorId::new(1), &first, &validators);
    signer
        .sign_state_recovery_checkpoint(&first, &validators)
        .unwrap();

    assert_eq!(
        store.next_state_recovery_checkpoint(),
        Err(
            second::PersistenceError::RecoveryCheckpointAwaitingFinality {
                validator_set_version: 7,
                serial: 1,
            }
        )
    );

    let premature_second =
        StateRecoveryCheckpoint::from_persisted(2, &store.load().unwrap().unwrap()).unwrap();
    assert_eq!(
        signer.sign_state_recovery_checkpoint(&premature_second, &validators),
        Err(ValidatorSigningError::Persistence(
            second::PersistenceError::RecoveryCheckpointAwaitingFinality {
                validator_set_version: 7,
                serial: 1,
            }
        ))
    );

    let statement = first.finality_statement();
    let votes = [1_u64, 2, 3]
        .into_iter()
        .map(|id| support::signed_vote(&statement, ValidatorId::new(id), &key((id * 3 + 1) as u8)))
        .collect();
    let certified = CertifiedStateRecoveryCheckpoint::new(first, votes, &validators).unwrap();
    store.advance_recovery_checkpoint_floor(&certified).unwrap();

    let second = store.next_state_recovery_checkpoint().unwrap();
    assert_eq!(second, premature_second);
    mark_recovery_finality_ready(&store, ValidatorId::new(1), &second, &validators);
    signer
        .sign_state_recovery_checkpoint(&second, &validators)
        .unwrap();

    store.remove_files().unwrap();
}

#[test]
fn certified_recovery_can_supersede_uncertified_local_vote_same_serial() {
    let validators = validator_set(7, 1..=4);
    let store = StateStore::new(temp_base("recovery-certified-supersedes-local-vote"));
    let mut state = SecondState::genesis([support::account(1)], 1);
    store.initialize(&state, &validators).unwrap();

    let signer = ValidatorSigner::new(ValidatorId::new(1), key(4), store.clone());
    let local =
        StateRecoveryCheckpoint::from_persisted(1, &store.load().unwrap().unwrap()).unwrap();
    mark_recovery_finality_ready(&store, ValidatorId::new(1), &local, &validators);
    signer
        .sign_state_recovery_checkpoint(&local, &validators)
        .unwrap();

    let task = verified_task(
        499,
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

    let finalized =
        StateRecoveryCheckpoint::from_persisted(1, &store.load().unwrap().unwrap()).unwrap();
    assert_ne!(local.digest(), finalized.digest());
    let statement = finalized.finality_statement();
    let votes = [2_u64, 3, 4]
        .into_iter()
        .map(|id| support::signed_vote(&statement, ValidatorId::new(id), &key((id * 3 + 1) as u8)))
        .collect();
    let certified = CertifiedStateRecoveryCheckpoint::new(finalized, votes, &validators).unwrap();

    store.advance_recovery_checkpoint_floor(&certified).unwrap();

    let next = store.next_state_recovery_checkpoint().unwrap();
    assert_eq!(next.serial(), 2);
    mark_recovery_finality_ready(&store, ValidatorId::new(1), &next, &validators);
    signer
        .sign_state_recovery_checkpoint(&next, &validators)
        .unwrap();

    store.remove_files().unwrap();
}

#[test]
fn recovery_checkpoint_vote_lock_blocks_same_serial_after_committed_state_changes() {
    let validators = validator_set(7, 1..=4);
    let store = StateStore::new(temp_base("recovery-serial-lock"));
    let mut state = SecondState::genesis([support::account(1)], 1);
    store.initialize(&state, &validators).unwrap();

    let signer = ValidatorSigner::new(ValidatorId::new(1), key(4), store.clone());
    let first =
        StateRecoveryCheckpoint::from_persisted(1, &store.load().unwrap().unwrap()).unwrap();
    mark_recovery_finality_ready(&store, ValidatorId::new(1), &first, &validators);
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
        StateRecoveryCheckpoint::from_persisted(1, &store.load().unwrap().unwrap()).unwrap();
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

#[test]
fn recovery_checkpoint_serial_floor_survives_restart_and_rejects_lower_serial() {
    let validators = validator_set(7, 1..=4);
    let base = temp_base("recovery-serial-floor");
    let store = StateStore::new(&base);
    let state = SecondState::genesis([], 1).with_reserve(1).unwrap();
    store.initialize(&state, &validators).unwrap();

    let accepted =
        StateRecoveryCheckpoint::from_persisted(20, &store.load().unwrap().unwrap()).unwrap();
    let statement = accepted.finality_statement();
    let votes = [1_u64, 2, 3]
        .into_iter()
        .map(|id| support::signed_vote(&statement, ValidatorId::new(id), &key((id * 3 + 1) as u8)))
        .collect();
    let certified =
        CertifiedStateRecoveryCheckpoint::new(accepted.clone(), votes, &validators).unwrap();
    store.advance_recovery_checkpoint_floor(&certified).unwrap();

    let restarted_store = StateStore::new(&base);
    let restarted_signer =
        ValidatorSigner::new(ValidatorId::new(1), key(4), restarted_store.clone());

    let replay =
        StateRecoveryCheckpoint::from_persisted(20, &restarted_store.load().unwrap().unwrap())
            .unwrap();
    mark_recovery_finality_ready(&restarted_store, ValidatorId::new(1), &replay, &validators);
    restarted_signer
        .sign_state_recovery_checkpoint(&replay, &validators)
        .unwrap();

    let stale =
        StateRecoveryCheckpoint::from_persisted(19, &restarted_store.load().unwrap().unwrap())
            .unwrap();
    assert!(matches!(
        restarted_signer.sign_state_recovery_checkpoint(&stale, &validators),
        Err(ValidatorSigningError::Persistence(
            second::PersistenceError::StaleRecoveryCheckpointSerial {
                validator_set_version: 7,
                minimum: 20,
                actual: 19,
            }
        ))
    ));

    let next = restarted_store.next_state_recovery_checkpoint().unwrap();
    assert_eq!(next.serial(), 21);
    mark_recovery_finality_ready(&restarted_store, ValidatorId::new(1), &next, &validators);
    restarted_signer
        .sign_state_recovery_checkpoint(&next, &validators)
        .unwrap();

    restarted_store.remove_files().unwrap();
}

#[test]
fn accepted_recovery_checkpoint_locks_same_serial_digest_without_local_vote() {
    let validators = validator_set(7, 1..=4);
    let store = StateStore::new(temp_base("recovery-certified-floor-digest"));
    let mut state = SecondState::genesis([support::account(1)], 1);
    store.initialize(&state, &validators).unwrap();

    let accepted =
        StateRecoveryCheckpoint::from_persisted(41, &store.load().unwrap().unwrap()).unwrap();
    let accepted_statement = accepted.finality_statement();
    let accepted_votes = [1_u64, 2, 3]
        .into_iter()
        .map(|id| {
            support::signed_vote(
                &accepted_statement,
                ValidatorId::new(id),
                &key((id * 3 + 1) as u8),
            )
        })
        .collect();
    let certified =
        CertifiedStateRecoveryCheckpoint::new(accepted.clone(), accepted_votes, &validators)
            .unwrap();
    store.advance_recovery_checkpoint_floor(&certified).unwrap();

    let task = verified_task(
        501,
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
        StateRecoveryCheckpoint::from_persisted(41, &store.load().unwrap().unwrap()).unwrap();
    assert_ne!(accepted.digest(), conflicting.digest());

    let signer = ValidatorSigner::new(ValidatorId::new(4), key(13), store.clone());
    assert!(matches!(
        signer.sign_state_recovery_checkpoint(&conflicting, &validators),
        Err(ValidatorSigningError::Persistence(
            second::PersistenceError::RecoveryCheckpointFloorConflict {
                validator_set_version: 7,
                serial: 41,
                ..
            }
        ))
    ));

    store.remove_files().unwrap();
}

#[test]
fn recovery_checkpoint_floor_is_scoped_by_validator_set_version() {
    let validators_v7 = validator_set(7, 1..=4);
    let validators_v8 = validator_set(8, 1..=4);
    let store = StateStore::new(temp_base("recovery-floor-set-scope"));
    let state = SecondState::genesis([], 1).with_reserve(1).unwrap();
    store.initialize(&state, &validators_v7).unwrap();

    let v7_checkpoint =
        StateRecoveryCheckpoint::from_persisted(20, &store.load().unwrap().unwrap()).unwrap();
    let statement = v7_checkpoint.finality_statement();
    let votes = [1_u64, 2, 3]
        .into_iter()
        .map(|id| support::signed_vote(&statement, ValidatorId::new(id), &key((id * 3 + 1) as u8)))
        .collect();
    let v7_certified =
        CertifiedStateRecoveryCheckpoint::new(v7_checkpoint, votes, &validators_v7).unwrap();
    store
        .advance_recovery_checkpoint_floor(&v7_certified)
        .unwrap();

    let registry = store.load().unwrap().unwrap().validator_registry;
    let transition = ValidatorSetTransition::new(
        CURRENT_PROTOCOL_VERSION,
        &validators_v7,
        &registry,
        validators_v8.clone(),
        vec![],
        vec![],
    )
    .unwrap();
    let statement = transition.finality_statement();
    let votes = [1_u64, 2, 3]
        .into_iter()
        .map(|id| support::signed_vote(&statement, ValidatorId::new(id), &key((id * 3 + 1) as u8)))
        .collect();
    let certified =
        CertifiedValidatorSetTransition::new(transition, votes, &validators_v7).unwrap();
    store.activate_validator_set_transition(&certified).unwrap();

    let v8_checkpoint = store.next_state_recovery_checkpoint().unwrap();
    assert_eq!(v8_checkpoint.serial(), 1);
    mark_recovery_finality_ready(&store, ValidatorId::new(1), &v8_checkpoint, &validators_v8);
    ValidatorSigner::new(ValidatorId::new(1), key(4), store.clone())
        .sign_state_recovery_checkpoint(&v8_checkpoint, &validators_v8)
        .unwrap();

    store.remove_files().unwrap();
}
