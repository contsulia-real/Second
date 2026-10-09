use super::tests::{key, temp_store, validator_set};
use super::*;
use crate::currency::Currency;
use crate::payment::PaymentAddressRecord;
use crate::{
    AuthorizerSet, BftDriver, BftDriverAction, BftPhase, BftProposalSubject, BftQuorumCertificate,
    BftStatement, BftValue, BftVote, CertifiedStateRecoveryCheckpoint,
    CertifiedValidatorSetTransition, ConsensusScope, CurrencyAddress, CurrencyRole,
    LegalTaskPayload, PaymentAddressStatus, StateRecoveryCheckpoint, StateRecoveryPayload,
    ValidatorSetTransition,
};

fn signed(name: &str, operations: Vec<Operation>) -> VerifiedLegalTask {
    let authorizers = AuthorizerSet::new(
        CURRENT_PROTOCOL_VERSION,
        [key(9).verifying_key().to_bytes()],
    )
    .unwrap();
    let mut task = crate::test_helpers::sign(
        LegalTaskPayload::new(
            TaskId::parse(name).unwrap(),
            CURRENT_PROTOCOL_VERSION,
            None,
            operations,
        ),
        &key(9),
    )
    .unwrap();
    task.add_account_signature(&key(82)).unwrap();
    task.verify(&authorizers).unwrap()
}

fn votes(statement: &FinalityStatement) -> Vec<ValidatorVote> {
    (1..=3)
        .map(|id| {
            ValidatorVote::sign_unchecked(statement, ValidatorId::new(id), &key((id * 3 + 1) as u8))
        })
        .collect()
}

fn qc(task_id: TaskId, digest: [u8; 32]) -> BftQuorumCertificate {
    let statement = BftStatement::new(
        1,
        ConsensusScope::PreparedTask(task_id),
        0,
        BftPhase::Precommit,
        BftValue::Digest(digest),
    );
    BftQuorumCertificate::new(
        statement.clone(),
        (1..=3)
            .map(|id| {
                BftVote::sign_unchecked(&statement, ValidatorId::new(id), &key((id * 3 + 1) as u8))
            })
            .collect(),
        &validator_set(),
    )
    .unwrap()
}

#[test]
fn certified_abort_releases_all_resources_atomically_retains_ranges_and_survives_shared_recovery() {
    let validators = validator_set();
    let alice = crate::test_helpers::account(81);
    let bob = crate::test_helpers::account(82);
    let account = crate::test_helpers::account(83);
    let source = PaymentAddress::from_bytes([81; 32]);
    let destination = PaymentAddress::from_bytes([82; 32]);
    let retiring = PaymentAddress::from_bytes([83; 32]);
    let mut state = SecondState::genesis([alice, bob], 2);
    for (address, owner) in [(source, alice), (destination, bob), (retiring, bob)] {
        state.business.payment_addresses.insert(
            address,
            PaymentAddressRecord {
                account: owner,
                status: PaymentAddressStatus::Active,
            },
        );
    }
    let currency = CurrencyAddress::new(1);
    state.business.currencies.insert(
        currency,
        Currency {
            address: currency,
            owner: Some(alice),
            role: CurrencyRole::Circulation,
        },
    );
    let operations = vec![
        Operation::Transfer {
            source,
            destination,
            amount: 1,
        },
        Operation::RegisterAccount { account },
        Operation::RetirePaymentAddress { address: retiring },
        Operation::Issue {
            account: alice,
            count: 1,
        },
    ];
    let task = signed("abort-resources", operations.clone());
    let (store, base) = temp_store();
    store.initialize(&state, &validators).unwrap();
    let allocation = crate::CurrencyAllocation::new(&task, 1, 2).unwrap();
    let statement = allocation.finality_statement();
    store
        .install_currency_allocation(
            &allocation,
            &FinalityCertificate::new(statement, votes(&statement), &validators).unwrap(),
        )
        .unwrap();
    state = store.load().unwrap().unwrap().state;
    let mut book = PreparedTaskBook::new(store.clone()).unwrap();
    book.prepare(&mut state, &task, 1, &validators).unwrap();
    let abort = store.prepared_abort_statement(&task.task_id()).unwrap();
    let subject = BftProposalSubject::new(
        1,
        ConsensusScope::PreparedTask(task.task_id()),
        abort.subject_digest(),
    );
    let certificate = qc(task.task_id(), abort.subject_digest());
    let mut finality_votes = Vec::new();
    for id in 1..=3 {
        let signer =
            ValidatorSigner::new(ValidatorId::new(id), key((id * 3 + 1) as u8), store.clone());
        // A cancellation intention cannot sign finality before the same-scope precommit QC.
        assert!(
            signer
                .sign_prepared_abort(task.task_id(), &abort, &validators)
                .is_err()
        );
        let mut driver = BftDriver::new(
            signer.clone(),
            store.clone(),
            validators.clone(),
            subject.scope().clone(),
        )
        .unwrap();
        driver.register_subject(&subject).unwrap();
        assert!(matches!(
            driver.accept_quorum_certificate(&certificate).unwrap(),
            BftDriverAction::FinalityReady { .. }
        ));
        finality_votes.push(
            signer
                .sign_prepared_abort(task.task_id(), &abort, &validators)
                .unwrap(),
        );
    }
    let certificate = FinalityCertificate::new(abort, finality_votes, &validators).unwrap();
    drop(book);
    // Restart between finality voting and certificate installation keeps the claims.
    book = PreparedTaskBook::new(StateStore::new(&base)).unwrap();
    assert_eq!(book.claimed_currency_count(), 1);
    book.abort_certified(&mut state, task.task_id(), &certificate)
        .unwrap();
    assert_eq!(book.claimed_currency_count(), 0);
    assert!(state.task_cancelled(task.task_id()));
    assert_eq!(state.next_currency_address(), 3);
    assert_eq!(state.business.currencies[&currency].owner, Some(alice));
    assert!(!state.business.accounts.contains(&account));
    assert_eq!(
        state.business.payment_addresses[&retiring].status,
        PaymentAddressStatus::Active
    );
    assert!(state.prerequisite.payment_executions.is_empty());
    let snapshot = store.load().unwrap().unwrap();
    let binding = &snapshot.state.protocol.task_bindings[&task.task_id()];
    assert_eq!(binding.allocation, Some((2, 1)));
    assert!(binding.allocation_task.is_none() && binding.allocation_certificate.is_none());
    assert_eq!(snapshot.validator_vote_locks.len(), 3);
    assert!(snapshot.bft_local_states.is_empty() && snapshot.prepared_tasks.is_empty());
    assert_eq!(
        store
            .install_prepared_abort(&task.task_id(), &certificate)
            .unwrap(),
        snapshot.generation
    );
    assert_eq!(
        book.prepare(&mut state, &task, 1, &validators),
        Err(PreparationError::Execution(ExecutionError::TaskCancelled))
    );
    let changed = signed(
        "abort-resources",
        vec![Operation::RegisterAccount { account }],
    );
    assert_eq!(
        book.prepare(&mut state, &changed, 1, &validators),
        Err(PreparationError::Execution(
            ExecutionError::TaskIdAlreadyBound
        ))
    );

    // A newer committee cannot rewrite the old decision. Only terminal committee
    // context is shared; no private active claims or BFT metadata are exported.
    let next = ValidatorSet::new(2, validators.credentials().cloned()).unwrap();
    let transition = ValidatorSetTransition::new(
        CURRENT_PROTOCOL_VERSION,
        &validators,
        &snapshot.validator_registry,
        next.clone(),
        Vec::new(),
        Vec::new(),
        3,
    )
    .unwrap();
    let transition = store.prepare_validator_set_transition(transition).unwrap();
    let statement = transition.finality_statement();
    store
        .activate_validator_set_transition(
            &CertifiedValidatorSetTransition::new(transition, votes(&statement), &validators)
                .unwrap(),
        )
        .unwrap();
    let newer = store.load().unwrap().unwrap();
    assert_eq!(
        store
            .install_prepared_abort(&task.task_id(), &certificate)
            .unwrap(),
        newer.generation
    );
    let wrong_set = crate::task_abort::statement(&task.task_id(), task.request_digest(), &next);
    assert!(
        store
            .install_prepared_abort(
                &task.task_id(),
                &FinalityCertificate::new(wrong_set, votes(&wrong_set), &next).unwrap()
            )
            .is_err()
    );
    assert_eq!(store.load().unwrap().unwrap().generation, newer.generation);
    let checkpoint = StateRecoveryCheckpoint::from_persisted(1, &newer).unwrap();
    let certified = CertifiedStateRecoveryCheckpoint::new(
        checkpoint.clone(),
        votes(&checkpoint.finality_statement()),
        &next,
    )
    .unwrap();
    let payload = StateRecoveryPayload::decode_bytes(
        &StateRecoveryPayload::from_persisted(&newer)
            .unwrap()
            .encode_bytes()
            .unwrap(),
    )
    .unwrap();
    let (recovered, recovered_base) = temp_store();
    recovered
        .install_recovered_state(&payload, &certified, &next)
        .unwrap();
    assert!(
        recovered
            .load()
            .unwrap()
            .unwrap()
            .state
            .task_cancelled(task.task_id())
    );
    let generation = recovered.load().unwrap().unwrap().generation;
    assert_eq!(
        recovered
            .install_prepared_abort(&task.task_id(), &certificate)
            .unwrap(),
        generation
    );

    // The same resources can now be used by a fresh task, without reusing the burned range.
    let retry = signed("fresh-after-abort", operations[..3].to_vec());
    book = PreparedTaskBook::new(store.clone()).unwrap();
    state = newer.state;
    book.prepare(&mut state, &retry, 1, &next).unwrap();
    let statement = book.prepared_finality_statement(retry.task_id()).unwrap();
    book.commit(
        &mut state,
        retry.task_id(),
        &FinalityCertificate::new(statement, votes(&statement), &next).unwrap(),
    )
    .unwrap();
    assert_eq!(state.business.currencies[&currency].owner, Some(bob));
    assert_eq!(state.next_currency_address(), 3);
    for (store, base) in [(store, base), (recovered, recovered_base)] {
        store.remove_files().unwrap();
        let _ = std::fs::remove_file(base.with_extension("lock"));
    }
}

#[test]
fn abort_rejects_wrong_request_commit_finality_and_bad_quorum_without_a_write() {
    let validators = validator_set();
    let task = signed(
        "abort-finality-boundary",
        vec![Operation::RegisterAccount {
            account: crate::test_helpers::account(91),
        }],
    );
    let (store, base) = temp_store();
    let mut state = SecondState::genesis([], 1);
    store.initialize(&state, &validators).unwrap();
    let mut book = PreparedTaskBook::new(store.clone()).unwrap();
    book.prepare(&mut state, &task, 1, &validators).unwrap();
    let abort = store.prepared_abort_statement(&task.task_id()).unwrap();
    let valid_abort = FinalityCertificate::new(abort, votes(&abort), &validators).unwrap();
    let generation = store.load().unwrap().unwrap().generation;
    for bad in [
        FinalityCertificate::from_untrusted_parts(abort, votes(&abort)[..2].to_vec()),
        FinalityCertificate::from_untrusted_parts(
            abort,
            vec![ValidatorVote::from_untrusted_parts(ValidatorId::new(1), [0; 64]); 3],
        ),
        {
            let wrong = crate::task_abort::statement(&task.task_id(), [0; 32], &validators);
            FinalityCertificate::new(wrong, votes(&wrong), &validators).unwrap()
        },
        {
            let wrong = crate::task_abort::statement(
                &TaskId::parse("other-task").unwrap(),
                task.request_digest(),
                &validators,
            );
            FinalityCertificate::new(wrong, votes(&wrong), &validators).unwrap()
        },
        {
            let newer = ValidatorSet::new(2, validators.credentials().cloned()).unwrap();
            let wrong =
                crate::task_abort::statement(&task.task_id(), task.request_digest(), &newer);
            FinalityCertificate::new(wrong, votes(&wrong), &newer).unwrap()
        },
    ] {
        assert!(store.install_prepared_abort(&task.task_id(), &bad).is_err());
        assert_eq!(store.load().unwrap().unwrap().generation, generation);
    }
    let commit = book.prepared_finality_statement(task.task_id()).unwrap();
    let signer = ValidatorSigner::new(ValidatorId::new(1), key(4), store.clone());
    store
        .accept_bft_precommit_qc(
            ValidatorId::new(1),
            &qc(task.task_id(), commit.subject_digest()),
            &validators,
        )
        .unwrap();
    signer
        .sign_prepared_task(task.task_id(), &commit, &validators)
        .unwrap();
    let generation = store.load().unwrap().unwrap().generation;
    assert!(
        store
            .install_prepared_abort(&task.task_id(), &valid_abort)
            .is_err()
    );
    assert_eq!(store.load().unwrap().unwrap().generation, generation);
    let commit_certificate = FinalityCertificate::new(commit, votes(&commit), &validators).unwrap();
    store
        .finalize_prepared_task(
            &task.task_id(),
            commit.subject_digest(),
            &commit_certificate,
        )
        .unwrap();
    assert!(
        store
            .install_prepared_abort(&task.task_id(), &valid_abort)
            .is_err()
    );
    book.commit(&mut state, task.task_id(), &commit_certificate)
        .unwrap();
    let generation = store.load().unwrap().unwrap().generation;
    assert!(
        store
            .install_prepared_abort(&task.task_id(), &valid_abort)
            .is_err()
    );
    assert_eq!(store.load().unwrap().unwrap().generation, generation);
    store.remove_files().unwrap();
    let _ = std::fs::remove_file(base.with_extension("lock"));
}
