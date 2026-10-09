use super::*;
use crate::prepared::tests::{key, temp_store, validator_set};
use crate::{
    AuthorizerSet, CURRENT_PROTOCOL_VERSION, CertifiedValidatorSetTransition, FinalityCertificate,
    FinalityStatement, LegalTaskPayload, Operation, PersistenceError, PreparedTaskBook, TaskId,
    ValidatorId, ValidatorSetTransition, ValidatorVote,
};

fn votes(statement: &FinalityStatement) -> Vec<ValidatorVote> {
    (1..=3)
        .map(|id| {
            ValidatorVote::sign_unchecked(statement, ValidatorId::new(id), &key((id * 3 + 1) as u8))
        })
        .collect()
}

#[test]
fn unbound_business_state_is_committed_and_cannot_activate_on_a_different_baseline() {
    let validators = validator_set();
    let account = crate::test_helpers::account(164);
    let (source, base) = temp_store();
    source
        .initialize(&SecondState::genesis([account], 1), &validators)
        .unwrap();
    let snapshot = source.load().unwrap().unwrap();
    assert!(snapshot.state.protocol.task_bindings.is_empty());
    let next = crate::ValidatorSet::new(2, validators.credentials().cloned()).unwrap();
    let bare = ValidatorSetTransition::new(
        CURRENT_PROTOCOL_VERSION,
        &validators,
        &snapshot.validator_registry,
        next,
        vec![],
        vec![],
        1,
    )
    .unwrap();
    let hydrated = source
        .prepare_validator_set_transition(bare.clone())
        .unwrap();
    assert!(hydrated.handoff_digest.is_some());
    let baseline = hydrated.handoff.as_ref().unwrap();
    let mut changed = snapshot.clone();
    changed.state.business.accounts.clear();
    assert_eq!(baseline.covers(&changed), Err(PersistenceError::StaleState));
    changed = snapshot.clone();
    changed.state.business.payment_addresses.insert(
        crate::PaymentAddress::from_bytes([164; 32]),
        crate::payment::PaymentAddressRecord {
            account,
            status: crate::PaymentAddressStatus::Active,
        },
    );
    assert_eq!(baseline.covers(&changed), Err(PersistenceError::StaleState));
    changed = snapshot.clone();
    changed.state = changed.state.with_reserve(1).unwrap();
    assert_eq!(baseline.covers(&changed), Err(PersistenceError::StaleState));
    let omitted = CertifiedValidatorSetTransition::new(
        bare.clone(),
        votes(&bare.finality_statement()),
        &validators,
    )
    .unwrap();
    assert_eq!(
        source.activate_validator_set_transition(&omitted),
        Err(PersistenceError::StalePreparedTasks)
    );
    assert_eq!(
        source.load().unwrap().unwrap().generation,
        snapshot.generation
    );
    let statement = hydrated.finality_statement();
    let certified =
        CertifiedValidatorSetTransition::new(hydrated, votes(&statement), &validators).unwrap();
    let (different, different_base) = temp_store();
    different
        .initialize(&SecondState::genesis([], 1), &validators)
        .unwrap();
    let before = different.load().unwrap().unwrap().generation;
    assert!(
        different
            .activate_validator_set_transition(&certified)
            .is_err()
    );
    assert_eq!(different.load().unwrap().unwrap().generation, before);
    source
        .activate_validator_set_transition(&certified)
        .unwrap();
    let cold = StateStore::new(&base).load().unwrap().unwrap();
    assert_eq!(cold.validator_set.version(), 2);
    assert!(cold.state.business.accounts.contains(&account));
    assert!(cold.state.protocol.task_handoff.is_some());
    let handoff = cold.state.protocol.task_handoff.as_ref().unwrap();
    let original_body = handoff.business_baseline_bytes().to_vec();
    let original_handoff = handoff.encode().unwrap();
    assert!(
        codec::decode_handoff_business_baseline(&original_body)
            .unwrap()
            .same_persisted_state(&snapshot.state)
    );
    let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
    let later_account = crate::test_helpers::account(165);
    let later = crate::test_helpers::sign(
        LegalTaskPayload::new(
            TaskId::parse("retained-baseline-later-account").unwrap(),
            1,
            None,
            vec![Operation::RegisterAccount {
                account: later_account,
            }],
        ),
        &key(9),
    )
    .unwrap()
    .verify(&authorizers)
    .unwrap();
    let mut current_state = cold.state.clone();
    let mut book = PreparedTaskBook::new(source.clone()).unwrap();
    book.prepare(&mut current_state, &later, 1, &cold.validator_set)
        .unwrap();
    let statement = book.prepared_finality_statement(later.task_id()).unwrap();
    let later_certificate =
        FinalityCertificate::new(statement, votes(&statement), &cold.validator_set).unwrap();
    book.commit(&mut current_state, later.task_id(), &later_certificate)
        .unwrap();
    let advanced = StateStore::new(&base).load().unwrap().unwrap();
    assert!(advanced.state.business.accounts.contains(&later_account));
    let retained = advanced.state.protocol.task_handoff.as_ref().unwrap();
    assert_eq!(retained.encode().unwrap(), original_handoff);
    let restored_baseline =
        codec::decode_handoff_business_baseline(retained.business_baseline_bytes()).unwrap();
    assert!(restored_baseline.business.accounts.contains(&account));
    assert!(!restored_baseline.business.accounts.contains(&later_account));
    assert!(restored_baseline.protocol.task_handoff.is_none());

    let mut reader = codec::Decoder::new(&original_handoff);
    assert_eq!(reader.read_u8().unwrap(), 1);
    validator_codec::decode_validator_set(&mut reader).unwrap();
    assert_eq!(reader.read_u8().unwrap(), 1);
    let digest_offset = original_handoff.len() - reader.remaining();
    reader.read_exact(32).unwrap();
    let length = reader.read_len().unwrap();
    let body_offset = original_handoff.len() - reader.remaining();
    let mut corrupt = original_handoff.clone();
    // The baseline ends with the no-handoff marker. Neither damaged bytes nor
    // a self-consistent digest may smuggle a recursively nested handoff.
    corrupt[body_offset + length - 1] = 1;
    assert_eq!(
        task_handoff::TaskHandoff::decode(&corrupt),
        Err(PersistenceError::InvalidSnapshot)
    );
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(b"SECOND_HANDOFF_BUSINESS_BASE_V1\0");
    hash.update(&corrupt[body_offset..body_offset + length]);
    corrupt[digest_offset..digest_offset + 32].copy_from_slice(&hash.finalize());
    assert_eq!(
        task_handoff::TaskHandoff::decode(&corrupt),
        Err(PersistenceError::InvalidSnapshot)
    );
    assert_eq!(
        StateStore::new(&base).load().unwrap().unwrap().generation,
        advanced.generation
    );
    for (store, path) in [(source, base), (different, different_base)] {
        store.remove_files().unwrap();
        let _ = std::fs::remove_file(path.with_extension("lock"));
    }
}

#[test]
fn certified_allocation_baseline_ignores_quorum_subset_but_handoff_carries_pending_body() {
    let validators = validator_set();
    let account = crate::test_helpers::account(163);
    let authorizers = AuthorizerSet::new(
        CURRENT_PROTOCOL_VERSION,
        [key(9).verifying_key().to_bytes()],
    )
    .unwrap();
    let task = crate::test_helpers::sign(
        LegalTaskPayload::new(
            TaskId::parse("baseline-allocation").unwrap(),
            CURRENT_PROTOCOL_VERSION,
            None,
            vec![Operation::Issue { account, count: 2 }],
        ),
        &key(9),
    )
    .unwrap()
    .verify(&authorizers)
    .unwrap();
    let allocation = crate::CurrencyAllocation::new(&task, 1, 1).unwrap();
    let statement = allocation.finality_statement();
    let (first, _) = temp_store();
    let (second, _) = temp_store();
    for (store, ids) in [(&first, 1..=3), (&second, 2..=4)] {
        store
            .initialize(&SecondState::genesis([account], 1), &validators)
            .unwrap();
        let votes = ids
            .map(|id| {
                ValidatorVote::sign_unchecked(
                    &statement,
                    ValidatorId::new(id),
                    &key((id * 3 + 1) as u8),
                )
            })
            .collect();
        store
            .install_currency_allocation(
                &allocation,
                &FinalityCertificate::new(statement, votes, &validators).unwrap(),
            )
            .unwrap();
    }
    second
        .finish_allocation_preparation(&task.task_id())
        .unwrap();
    let first = first.load().unwrap().unwrap();
    let mut second = second.load().unwrap().unwrap();
    assert!(
        first.state.protocol.task_bindings[&task.task_id()]
            .allocation_task
            .is_some()
    );
    assert!(
        second.state.protocol.task_bindings[&task.task_id()]
            .allocation_task
            .is_none()
    );
    let baseline = task_handoff::TaskHandoff::capture(&first).unwrap();
    let other = task_handoff::TaskHandoff::capture(&second).unwrap();
    assert!(baseline.requires_commitment());
    assert_eq!(
        baseline.business_baseline_bytes(),
        other.business_baseline_bytes()
    );
    assert_ne!(baseline.digest().unwrap(), other.digest().unwrap());
    baseline.covers(&second).unwrap();
    assert_eq!(
        other.covers(&first),
        Err(PersistenceError::StalePreparedTasks)
    );
    second
        .state
        .protocol
        .task_bindings
        .get_mut(&task.task_id())
        .unwrap()
        .request_digest[0] ^= 1;
    assert_eq!(baseline.covers(&second), Err(PersistenceError::StaleState));
}

#[test]
fn terminal_business_baseline_cannot_be_omitted_or_replaced_when_plans_are_empty() {
    let validators = validator_set();
    let (store, _) = temp_store();
    let mut state = SecondState::genesis([], 1);
    store.initialize(&state, &validators).unwrap();
    let authorizers = AuthorizerSet::new(
        CURRENT_PROTOCOL_VERSION,
        [key(9).verifying_key().to_bytes()],
    )
    .unwrap();
    let task = crate::test_helpers::sign(
        LegalTaskPayload::new(
            TaskId::parse("baseline-terminal").unwrap(),
            CURRENT_PROTOCOL_VERSION,
            None,
            vec![Operation::RegisterAccount {
                account: crate::test_helpers::account(161),
            }],
        ),
        &key(9),
    )
    .unwrap()
    .verify(&authorizers)
    .unwrap();
    let mut book = PreparedTaskBook::new(store.clone()).unwrap();
    book.prepare(&mut state, &task, 1, &validators).unwrap();
    let abort = store.prepared_abort_statement(&task.task_id()).unwrap();
    book.abort_certified(
        &mut state,
        task.task_id(),
        &FinalityCertificate::new(abort, votes(&abort), &validators).unwrap(),
    )
    .unwrap();
    let snapshot = store.load().unwrap().unwrap();
    assert!(snapshot.prepared_tasks.is_empty());
    let next = crate::ValidatorSet::new(2, validators.credentials().cloned()).unwrap();
    let bare = ValidatorSetTransition::new(
        CURRENT_PROTOCOL_VERSION,
        &validators,
        &snapshot.validator_registry,
        next,
        vec![],
        vec![],
        1,
    )
    .unwrap();
    let statement = bare.finality_statement();
    let omitted =
        CertifiedValidatorSetTransition::new(bare.clone(), votes(&statement), &validators).unwrap();
    assert_eq!(
        store.activate_validator_set_transition(&omitted),
        Err(crate::PersistenceError::StalePreparedTasks)
    );
    assert_eq!(
        store.load().unwrap().unwrap().generation,
        snapshot.generation
    );
    let hydrated = store.prepare_validator_set_transition(bare).unwrap();
    assert!(hydrated.handoff_digest.is_some());
    let mut changed = snapshot.clone();
    changed
        .state
        .business
        .accounts
        .insert(crate::test_helpers::account(162));
    assert_eq!(
        task_handoff::hydrate(&changed, &hydrated),
        Err(crate::PersistenceError::StaleState)
    );
    changed = snapshot.clone();
    changed
        .state
        .protocol
        .task_bindings
        .get_mut(&task.task_id())
        .unwrap()
        .request_digest[0] ^= 1;
    assert_eq!(
        task_handoff::hydrate(&changed, &hydrated),
        Err(crate::PersistenceError::StaleState)
    );
    let (stale, _) = temp_store();
    stale
        .initialize(&SecondState::genesis([], 1), &validators)
        .unwrap();
    let source = crate::ValidatorSetTransitionSource::from_transition(&hydrated);
    let stale_snapshot = stale.load().unwrap().unwrap();
    let proposal = source
        .verify(&validators, &stale_snapshot.validator_registry)
        .unwrap();
    assert!(stale.prepare_validator_set_transition(proposal).is_err());
    assert_eq!(
        stale.load().unwrap().unwrap().generation,
        stale_snapshot.generation
    );
    let statement = hydrated.finality_statement();
    let certified =
        CertifiedValidatorSetTransition::new(hydrated, votes(&statement), &validators).unwrap();
    store.activate_validator_set_transition(&certified).unwrap();
    assert!(
        store
            .load()
            .unwrap()
            .unwrap()
            .state
            .task_cancelled(task.task_id())
    );
    let mut cached = store.load().unwrap().unwrap();
    let proof = cached.validator_transition_proofs.get(&1).unwrap();
    let mut bytes = proof.encode_bytes().unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    cached.validator_transition_proofs.insert(
        1,
        crate::ValidatorSetTransitionProof::decode_bytes(&bytes).unwrap(),
    );
    assert_eq!(
        task_handoff::validate_installed(
            &cached.state,
            &cached.validator_set,
            &cached.validator_transition_proofs,
            None
        ),
        Err(crate::PersistenceError::InvalidSnapshot)
    );
}
