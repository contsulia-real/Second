//! A collecting handoff must follow already certified business and frontier progress.
use crate::prepared::tests::{key, temp_store, validator_set};
use crate::*;

#[test]
fn collection_accepts_late_local_duties_but_sealed_root_closes_ownership() {
    let validators = validator_set();
    let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
    let request = |name: &str, byte| {
        crate::test_helpers::sign(
            LegalTaskPayload::new(
                TaskId::parse(name).unwrap(),
                1,
                None,
                vec![Operation::RegisterAccount {
                    account: crate::test_helpers::account(byte),
                }],
            ),
            &key(9),
        )
        .unwrap()
        .verify(&authorizers)
        .unwrap()
    };
    let (store, base) = temp_store();
    let mut state = SecondState::genesis([], 1);
    store.initialize(&state, &validators).unwrap();
    let snapshot = store.load().unwrap().unwrap();
    let transition = ValidatorSetTransition::new(
        1,
        &validators,
        &snapshot.validator_registry,
        ValidatorSet::new(2, validators.credentials().cloned()).unwrap(),
        vec![],
        vec![],
        1,
    )
    .unwrap();
    let transition = store.prepare_validator_set_transition(transition).unwrap();
    let mut book = PreparedTaskBook::new(store.clone()).unwrap();
    let initial = book
        .collect_transition_handoff(&mut state, &transition, &authorizers, 1)
        .unwrap();
    let late = request("late-local-collection-duty", 204);
    book.prepare(&mut state, &late, 1, &validators).unwrap();
    let cold = StateStore::new(&base).load().unwrap().unwrap();
    let pending = cold.pending_governance.values().next().unwrap();
    assert!(pending.target().is_none());
    let refreshed = pending.transition().unwrap().clone();
    assert_ne!(refreshed.digest(), initial.digest());
    refreshed.handoff.as_ref().unwrap().covers(&cold).unwrap();
    assert!(
        refreshed
            .handoff
            .as_ref()
            .unwrap()
            .task_context(&late.task_id())
            .is_some()
    );
    assert!(cold.prepared_tasks[&late.task_id()].commit_authorized);
    assert!(cold.validator_vote_locks.is_empty());
    assert!(cold.bft_local_states.is_empty());
    let generation = cold.generation;
    assert_eq!(
        store.promote_transition_collection(&initial),
        Err(PersistenceError::StalePreparedTasks)
    );
    assert_eq!(store.load().unwrap().unwrap().generation, generation);
    assert!(store.promote_transition_collection(&refreshed).unwrap());
    let generation = store.load().unwrap().unwrap().generation;
    let after_seal = request("after-sealed-collection-duty", 205);
    assert!(matches!(
        book.prepare(&mut state, &after_seal, 1, &validators),
        Err(PreparationError::Persistence(
            PersistenceError::TaskAdmissionClosed { .. }
        ))
    ));
    let cold = StateStore::new(&base).load().unwrap().unwrap();
    assert_eq!(cold.generation, generation);
    assert!(!cold.prepared_tasks.contains_key(&after_seal.task_id()));
    refreshed.handoff.as_ref().unwrap().covers(&cold).unwrap();
    assert!(cold.validator_vote_locks.is_empty());
    assert!(cold.bft_local_states.is_empty());
    // A late, valid foreign witness carries no ownership, but an Abort vote
    // would still decide its request. It cannot bypass the sealed root.
    let (provider, provider_base) = temp_store();
    let mut provider_state = SecondState::genesis([], 1);
    provider.initialize(&provider_state, &validators).unwrap();
    PreparedTaskBook::new(provider.clone())
        .unwrap()
        .prepare(&mut provider_state, &after_seal, 1, &validators)
        .unwrap();
    let contribution = refreshed
        .clone()
        .with_handoff(
            crate::persistence::TaskHandoff::capture(&provider.load().unwrap().unwrap()).unwrap(),
        )
        .unwrap();
    book.collect_transition_handoff(&mut state, &contribution, &authorizers, 1)
        .unwrap();
    let witnessed = store.load_shared().unwrap().unwrap();
    assert!(!witnessed.prepared_tasks[&after_seal.task_id()].commit_authorized);
    let advertised = super::super::governance::PendingGovernance::transition_sources(
        &witnessed.pending_governance,
    );
    assert_eq!(advertised.len(), 1);
    assert_eq!(
        advertised[0]
            .transition()
            .unwrap()
            .handoff
            .as_ref()
            .unwrap()
            .plans
            .len(),
        2
    );
    assert!(
        witnessed
            .pending_governance
            .contains_key(&refreshed.digest()),
        "old sealed roots remain available for exact proof/body requests"
    );
    let generation = witnessed.generation;
    let signer = ValidatorSigner::new(ValidatorId::new(1), key(4), store.clone());
    let abort = store
        .prepared_abort_statement(&after_seal.task_id())
        .unwrap();
    assert!(
        matches!(
            signer.sign_bft_prevote(
                ConsensusScope::PreparedTask(after_seal.task_id()),
                0,
                BftValue::Digest(abort.subject_digest()),
                &validators,
                None,
            ),
            Err(ValidatorSigningError::Persistence(
                PersistenceError::TaskAdmissionClosed { .. }
            ))
        ),
        "an unlisted late witness obtained an old-committee Abort vote after sealing"
    );
    assert_eq!(store.load_shared().unwrap().unwrap().generation, generation);
    let covered = store.prepared_bft_proposal_subject(late.task_id()).unwrap();
    signer
        .sign_bft_prevote(
            covered.scope().clone(),
            0,
            BftValue::Digest(covered.digest()),
            &validators,
            None,
        )
        .unwrap();
    // Receiving late quorum evidence is distinct from permission to sign it.
    // An admitted root must not use the foreign-witness exception to omit Abort.
    let statement = BftStatement::new(
        1,
        ConsensusScope::PreparedTask(after_seal.task_id()),
        0,
        BftPhase::Precommit,
        BftValue::Digest(abort.subject_digest()),
    );
    let qc = BftQuorumCertificate::new(
        statement.clone(),
        (1..=3)
            .map(|id| {
                BftVote::sign_unchecked(&statement, ValidatorId::new(id), &key((id * 3 + 1) as u8))
            })
            .collect(),
        &validators,
    )
    .unwrap();
    store
        .accept_bft_precommit_qc(ValidatorId::new(4), &qc, &validators)
        .unwrap();
    let protected = StateStore::new(&base).load().unwrap().unwrap();
    assert_eq!(
        refreshed.handoff.as_ref().unwrap().covers(&protected),
        Err(PersistenceError::StalePreparedTasks)
    );
    provider.remove_files().unwrap();
    let _ = std::fs::remove_file(provider_base.with_extension("lock"));
    store.remove_files().unwrap();
    let _ = std::fs::remove_file(base.with_extension("lock"));
}

#[test]
fn collecting_handoff_refreshes_atomically_after_late_business_and_allocation_certificates() {
    let validators = validator_set();
    let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
    let account = crate::test_helpers::account(201);
    let completed_account = crate::test_helpers::account(202);
    let remaining_account = crate::test_helpers::account(203);
    let sign = |name: &str, operations| {
        crate::test_helpers::sign(
            LegalTaskPayload::new(TaskId::parse(name).unwrap(), 1, None, operations),
            &key(9),
        )
        .unwrap()
        .verify(&authorizers)
        .unwrap()
    };
    let completed = sign(
        "collection-late-business",
        vec![Operation::RegisterAccount {
            account: completed_account,
        }],
    );
    let remaining = sign(
        "collection-remaining",
        vec![Operation::RegisterAccount {
            account: remaining_account,
        }],
    );
    let issue = sign(
        "collection-late-allocation",
        vec![Operation::Issue { account, count: 1 }],
    );
    let (store, base) = temp_store();
    let mut state = SecondState::genesis([account], 1);
    store.initialize(&state, &validators).unwrap();
    store.queue_currency_allocation(&issue).unwrap();
    state = store.load().unwrap().unwrap().state;
    let mut book = PreparedTaskBook::new(store.clone()).unwrap();
    for task in [&completed, &remaining] {
        book.prepare(&mut state, task, 1, &validators).unwrap();
    }
    let certificate = |statement| {
        FinalityCertificate::new(
            statement,
            (1..=3)
                .map(|id| {
                    ValidatorVote::sign_unchecked(
                        &statement,
                        ValidatorId::new(id),
                        &key((id * 3 + 1) as u8),
                    )
                })
                .collect(),
            &validators,
        )
        .unwrap()
    };
    let business_certificate = certificate(
        book.prepared_finality_statement(completed.task_id())
            .unwrap(),
    );
    let precommit = BftStatement::new(
        1,
        ConsensusScope::PreparedTask(completed.task_id()),
        0,
        BftPhase::Precommit,
        BftValue::Digest(business_certificate.statement().subject_digest()),
    );
    let precommit = BftQuorumCertificate::new(
        precommit.clone(),
        (1..=3)
            .map(|id| {
                BftVote::sign_unchecked(&precommit, ValidatorId::new(id), &key((id * 3 + 1) as u8))
            })
            .collect(),
        &validators,
    )
    .unwrap();
    store
        .accept_bft_precommit_qc(ValidatorId::new(1), &precommit, &validators)
        .unwrap();
    book = PreparedTaskBook::new(store.clone()).unwrap();
    assert_eq!(
        book.sign_prepared_vote(completed.task_id(), ValidatorId::new(1), &key(4))
            .unwrap(),
        business_certificate.votes()[0],
    );
    let allocation = CurrencyAllocation::new(&issue, 1, 1).unwrap();
    let allocation_certificate = certificate(allocation.finality_statement());
    let snapshot = store.load().unwrap().unwrap();
    let transition = store
        .prepare_validator_set_transition(
            ValidatorSetTransition::new(
                1,
                &validators,
                &snapshot.validator_registry,
                ValidatorSet::new(2, validators.credentials().cloned()).unwrap(),
                vec![],
                vec![],
                1,
            )
            .unwrap(),
        )
        .unwrap();
    let collected = book
        .collect_transition_handoff(&mut state, &transition, &authorizers, 1)
        .unwrap();
    assert_eq!(collected.handoff.as_ref().unwrap().plans.len(), 2);
    let with_queued = store.load().unwrap().unwrap();
    let mut without_queued = with_queued.clone();
    without_queued
        .state
        .protocol
        .task_bindings
        .remove(&issue.task_id());
    assert_ne!(
        collected.handoff.as_ref().unwrap().digest().unwrap(),
        crate::persistence::TaskHandoff::capture(&without_queued)
            .unwrap()
            .digest()
            .unwrap(),
        "the accepted unprepared request is missing from the handoff commitment"
    );
    let decoded = crate::persistence::TaskHandoff::decode(
        &collected.handoff.as_ref().unwrap().encode().unwrap(),
    )
    .unwrap();
    assert_eq!(
        decoded.requests.get(&issue.task_id()),
        Some(issue.signed_task())
    );
    assert!(!decoded.plans.keys().any(|(id, _)| *id == issue.task_id()));
    // These proofs existed before collection; their delayed delivery remains valid.
    book.commit(&mut state, completed.task_id(), &business_certificate)
        .unwrap();
    let cold = StateStore::new(&base).load().unwrap().unwrap();
    assert_eq!(cold.state.task_succeeded(completed.task_id()), Some(true));
    assert!(cold.state.business.accounts.contains(&completed_account));
    assert_eq!(cold.pending_governance.len(), 1);
    let refreshed = cold
        .pending_governance
        .values()
        .next()
        .unwrap()
        .transition()
        .unwrap();
    refreshed.handoff.as_ref().unwrap().covers(&cold).unwrap();
    assert_ne!(refreshed.digest(), collected.digest());
    assert_eq!(refreshed.handoff.as_ref().unwrap().plans.len(), 1);
    assert!(
        refreshed
            .handoff
            .as_ref()
            .unwrap()
            .task_context(&remaining.task_id())
            .is_some()
    );
    assert!(cold.prepared_tasks[&remaining.task_id()].commit_authorized);
    assert_eq!(
        cold.task_receipts[&completed.task_id()]
            .certificate()
            .unwrap(),
        business_certificate
    );

    store
        .install_currency_allocation(&allocation, &allocation_certificate)
        .unwrap();
    let advanced = StateStore::new(&base).load().unwrap().unwrap();
    assert_eq!(advanced.state.next_currency_address(), 2);
    assert_eq!(
        advanced.pending_governance.len(),
        1,
        "frontier advance must not discard the collection intent"
    );
    let pending = advanced.pending_governance.values().next().unwrap();
    assert!(pending.target().is_none());
    let moved = pending.transition().unwrap();
    assert_eq!(moved.currency_frontier(), 2);
    assert_eq!(moved.next_validator_set(), refreshed.next_validator_set());
    moved.handoff.as_ref().unwrap().covers(&advanced).unwrap();
    assert_eq!(moved.handoff.as_ref().unwrap().plans.len(), 1);
    assert!(advanced.prepared_tasks[&remaining.task_id()].commit_authorized);
    assert!(advanced.bft_local_states.is_empty());
    assert_eq!(
        advanced.validator_vote_locks.get(&(
            ValidatorId::new(1),
            ConsensusScope::PreparedTask(completed.task_id()),
        )),
        Some(&business_certificate.statement().subject_digest()),
        "refreshing an unsigned collection must preserve the original irreversible vote lock"
    );
    let generation = advanced.generation;
    let signer = ValidatorSigner::new(ValidatorId::new(1), key(4), store.clone());
    // The stable collection intent is only a bounded registration key. It
    // cannot become a digest vote while the canonical root remains collecting.
    for digest in [
        moved.digest(),
        moved.clone().with_handoff_digest(None).digest(),
    ] {
        assert!(matches!(
            signer.sign_bft_prevote(
                moved.scope(),
                0,
                BftValue::Digest(digest),
                &validators,
                None
            ),
            Err(ValidatorSigningError::Persistence(
                PersistenceError::TransitionCollectionIncomplete { .. }
            ))
        ));
    }
    assert_eq!(store.load().unwrap().unwrap().generation, generation);
    assert!(matches!(
        store.admit_governance(
            &crate::runtime_consensus_target::ValidatorConsensusTarget::ValidatorSetTransition(
                moved.clone()
            )
        ),
        Err(PersistenceError::TransitionCollectionIncomplete { .. })
    ));
    store
        .install_currency_allocation(&allocation, &allocation_certificate)
        .unwrap();
    assert_eq!(store.load().unwrap().unwrap().generation, generation);
    store.remove_files().unwrap();
    let _ = std::fs::remove_file(base.with_extension("lock"));
}
