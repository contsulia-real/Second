use super::*;
use crate::prepared::tests::{key, temp_store, validator_set};
use crate::{AuthorizerSet, CurrencyAddress, LegalTaskPayload, ValidatorSetTransition};

mod variant_capacity;

#[test]
fn collection_imports_more_obligations_than_the_unregistered_message_window() {
    let validators = validator_set();
    let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
    let (provider, provider_base) = temp_store();
    let (receiver, receiver_base) = temp_store();
    let initial = SecondState::genesis([], 1);
    for store in [&provider, &receiver] {
        store.initialize(&initial, &validators).unwrap();
    }
    let task = |name: &str, byte| {
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
    let local = task("local-capacity-obligation", 220);
    let mut local_state = initial.clone();
    let mut local_book = PreparedTaskBook::new(receiver.clone()).unwrap();
    local_book
        .prepare(&mut local_state, &local, 1, &validators)
        .unwrap();
    let mut remote_state = initial;
    let mut remote_book = PreparedTaskBook::new(provider.clone()).unwrap();
    let remote_count = crate::runtime_bft_consensus::MAX_PENDING_UNREGISTERED_SCOPES + 1;
    for index in 0..remote_count {
        let remote = task(&format!("remote-capacity-{index}"), index as u8);
        remote_book
            .prepare(&mut remote_state, &remote, 1, &validators)
            .unwrap();
    }
    let snapshot = provider.load().unwrap().unwrap();
    let transition = ValidatorSetTransition::new(
        1,
        &validators,
        &snapshot.validator_registry,
        ValidatorSet::new(2, validators.credentials().cloned()).unwrap(),
        Vec::new(),
        Vec::new(),
        1,
    )
    .unwrap()
    .with_handoff(crate::persistence::TaskHandoff::capture(&snapshot).unwrap())
    .unwrap();
    let collected = local_book
        .collect_transition_handoff(&mut local_state, &transition, &authorizers, 1)
        .unwrap();
    let cold = receiver.load().unwrap().unwrap();
    assert_eq!(cold.prepared_tasks.len(), remote_count + 1);
    assert!(cold.prepared_tasks[&local.task_id()].commit_authorized);
    assert!(
        cold.prepared_tasks
            .iter()
            .all(|(id, plan)| id == &local.task_id() || !plan.has_owned_candidate())
    );
    assert_eq!(
        collected.handoff.as_ref().unwrap().plans.len(),
        remote_count + 1
    );
    collected.handoff.as_ref().unwrap().covers(&cold).unwrap();
    assert!(cold.bft_local_states.is_empty());
    assert!(matches!(
        cold.pending_governance.get(&collected.digest()),
        Some(crate::persistence::PendingGovernance::CollectingTransition(
            _
        ))
    ));
    let repeated = local_book
        .collect_transition_handoff(&mut local_state, &collected, &authorizers, 1)
        .unwrap();
    assert_eq!(repeated.digest(), collected.digest());
    assert_eq!(
        receiver.load().unwrap().unwrap().generation,
        cold.generation
    );
    for (store, base) in [(provider, provider_base), (receiver, receiver_base)] {
        store.remove_files().unwrap();
        let _ = std::fs::remove_file(base.with_extension("lock"));
    }
}

#[tokio::test]
async fn handoff_collection_unions_local_obligations_and_keeps_expiry_and_atomic_admission() {
    let validators = validator_set();
    let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
    let alice = crate::test_helpers::account(171);
    let bob = crate::test_helpers::account(172);
    let source = PaymentAddress::from_bytes([171; 32]);
    let destination = PaymentAddress::from_bytes([172; 32]);
    let mut initial = SecondState::genesis([alice, bob], 3);
    for (address, account) in [(source, alice), (destination, bob)] {
        initial.business.payment_addresses.insert(
            address,
            crate::payment::PaymentAddressRecord {
                account,
                status: crate::PaymentAddressStatus::Active,
            },
        );
    }
    for value in 1..=2 {
        let address = CurrencyAddress::new(value);
        initial.business.currencies.insert(
            address,
            crate::currency::Currency {
                address,
                role: crate::CurrencyRole::Circulation,
                owner: Some(alice),
            },
        );
    }
    let sign = |name: &str, operations| {
        crate::test_helpers::sign(
            LegalTaskPayload::new(TaskId::parse(name).unwrap(), 1, Some(2), operations),
            &key(9),
        )
        .unwrap()
        .verify(&authorizers)
        .unwrap()
    };
    let transfer = sign(
        "aa-handoff-deadline",
        vec![Operation::Transfer {
            source,
            destination,
            amount: 1,
        }],
    );
    let expired_new = sign(
        "zz-handoff-expired-new",
        vec![Operation::RegisterAccount {
            account: crate::test_helpers::account(173),
        }],
    );
    let (provider, provider_base) = temp_store();
    let (receiver, receiver_base) = temp_store();
    let (fresh, fresh_base) = temp_store();
    for store in [&provider, &receiver] {
        store.initialize(&initial, &validators).unwrap();
    }
    let mut fresh_state = initial.clone();
    fresh_state.bind_task(&transfer).unwrap();
    fresh.initialize(&fresh_state, &validators).unwrap();
    for store in [&provider, &receiver] {
        PreparedTaskBook::new(store.clone())
            .unwrap()
            .prepare(&mut initial.clone(), &transfer, 1, &validators)
            .unwrap();
    }
    let mut provider_state = provider.load().unwrap().unwrap().state;
    let mut provider_book = PreparedTaskBook::new(provider.clone()).unwrap();
    let mut alternative = provider
        .load_prepared_tasks()
        .unwrap()
        .remove(&transfer.task_id())
        .unwrap();
    let crate::prepared_plan::PreparedOperation::Transfer { currencies, .. } =
        &mut alternative.operations[0]
    else {
        panic!("transfer fixture")
    };
    *currencies = vec![CurrencyAddress::new(2)]
        .into_iter()
        .collect::<crate::AddressRanges>();
    let digest = alternative.plan_digest().unwrap();
    provider_book
        .admit_frozen_variant(
            &mut provider_state,
            &transfer,
            &validators,
            digest,
            &[vec![CurrencyAddress::new(2)]
                .into_iter()
                .collect::<crate::AddressRanges>()],
        )
        .unwrap();
    let queued = sign(
        "handoff-late-queued",
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );
    provider.queue_currency_allocation(&queued).unwrap();
    provider_state = provider.load().unwrap().unwrap().state;
    let snapshot = provider.load().unwrap().unwrap();
    let bare = ValidatorSetTransition::new(
        1,
        &validators,
        &snapshot.validator_registry,
        ValidatorSet::new(2, validators.credentials().cloned()).unwrap(),
        vec![],
        vec![],
        3,
    )
    .unwrap();
    let transition = provider
        .prepare_validator_set_transition(bare.clone())
        .unwrap();
    let fresh_generation = fresh.load().unwrap().unwrap().generation;
    assert!(matches!(
        PreparedTaskBook::new(fresh.clone())
            .unwrap()
            .admit_transition_handoff(&mut fresh_state, &transition, &authorizers, 3),
        Err(PreparationError::Execution(ExecutionError::TaskExpired))
    ));
    assert_eq!(fresh.load().unwrap().unwrap().generation, fresh_generation);
    provider_book
        .prepare(&mut provider_state, &expired_new, 1, &validators)
        .unwrap();
    let with_expired_new = provider.prepare_validator_set_transition(bare).unwrap();
    let mut state = receiver.load().unwrap().unwrap().state;
    let local_only = crate::test_helpers::sign(
        LegalTaskPayload::new(
            TaskId::parse("handoff-receiver-only").unwrap(),
            1,
            None,
            vec![Operation::RegisterAccount {
                account: crate::test_helpers::account(174),
            }],
        ),
        &key(9),
    )
    .unwrap()
    .verify(&authorizers)
    .unwrap();
    PreparedTaskBook::new(receiver.clone())
        .unwrap()
        .prepare(&mut state, &local_only, 1, &validators)
        .unwrap();
    let generation = receiver.load().unwrap().unwrap().generation;
    let mut book = PreparedTaskBook::new(receiver.clone()).unwrap();
    assert!(matches!(
        book.admit_transition_handoff(&mut state, &with_expired_new, &authorizers, 3),
        Err(PreparationError::Execution(ExecutionError::TaskExpired))
    ));
    assert_eq!(receiver.load().unwrap().unwrap().generation, generation);
    assert!(
        receiver.load_prepared_tasks().unwrap()[&transfer.task_id()]
            .candidate(digest)
            .unwrap()
            .is_none()
    );
    // Final admission must still reject a root omitting this member's task.
    assert!(matches!(
        book.admit_transition_handoff(&mut state, &transition, &authorizers, 3),
        Err(PreparationError::Persistence(
            PersistenceError::StalePreparedTasks
        ))
    ));
    assert_eq!(receiver.load().unwrap().unwrap().generation, generation);
    let mut forged = transition.handoff.as_ref().unwrap().as_ref().clone();
    let (_, forged_plan) = forged.plans.iter_mut().next().unwrap();
    forged_plan.source_task = local_only.signed_task().clone();
    let forged_transition = transition.clone().with_handoff(forged).unwrap();
    assert!(
        book.collect_transition_handoff(&mut state, &forged_transition, &authorizers, 3)
            .is_err()
    );
    assert_eq!(receiver.load().unwrap().unwrap().generation, generation);
    let (sealed, sealed_base) = temp_store();
    sealed.initialize(&state, &validators).unwrap();
    sealed
        .replace_prepared_tasks(&Default::default(), &book.tasks)
        .unwrap();
    let sealed_snapshot = sealed.load().unwrap().unwrap();
    let seal = transition
        .clone()
        .with_handoff(crate::persistence::TaskHandoff::capture(&sealed_snapshot).unwrap())
        .unwrap();
    sealed
        .admit_governance(
            &crate::runtime_consensus_target::ValidatorConsensusTarget::ValidatorSetTransition(
                seal.clone(),
            ),
        )
        .unwrap();
    let sealed_generation = sealed.load().unwrap().unwrap().generation;
    let mut sealed_book = PreparedTaskBook::new(sealed.clone()).unwrap();
    let mut sealed_state = state.clone();
    assert_eq!(
        sealed_book
            .collect_transition_handoff(&mut sealed_state, &seal, &authorizers, 3)
            .unwrap()
            .digest(),
        seal.digest()
    );
    assert_eq!(
        sealed.load().unwrap().unwrap().generation,
        sealed_generation
    );
    let extended = sealed_book
        .collect_transition_handoff(&mut sealed_state, &transition, &authorizers, 3)
        .unwrap();
    let after_extension = StateStore::new(&sealed_base).load().unwrap().unwrap();
    assert_eq!(after_extension.generation, sealed_generation + 1);
    assert_ne!(extended.digest(), seal.digest());
    assert!(matches!(
        after_extension.pending_governance.get(&seal.digest()),
        Some(crate::persistence::PendingGovernance::Transition(value)) if value == &seal
    ));
    assert_eq!(
        after_extension.state.protocol.task_bindings[&queued.task_id()]
            .allocation_task
            .as_ref(),
        Some(queued.signed_task())
    );
    seal.handoff
        .as_ref()
        .unwrap()
        .covers(&after_extension)
        .unwrap();
    sealed
        .validator_transition_bft_proposal_subject(&seal)
        .unwrap();
    let signer = crate::ValidatorSigner::new(crate::ValidatorId::new(2), key(7), sealed.clone());
    let decision = crate::BftStatement::new(
        validators.version(),
        seal.scope(),
        0,
        crate::BftPhase::Precommit,
        crate::BftValue::Digest(seal.digest()),
    );
    let qc = crate::BftQuorumCertificate::new(
        decision.clone(),
        (1..=3)
            .map(|id| {
                crate::BftVote::sign_unchecked(
                    &decision,
                    crate::ValidatorId::new(id),
                    &key((id * 3 + 1) as u8),
                )
            })
            .collect(),
        &validators,
    )
    .unwrap();
    sealed
        .accept_bft_precommit_qc(crate::ValidatorId::new(2), &qc, &validators)
        .unwrap();
    signer
        .sign_validator_set_transition(&seal, &validators)
        .unwrap();
    let before_upgrade = sealed.load().unwrap().unwrap().generation;
    assert!(matches!(
        sealed_book.admit_frozen_variant(
            &mut sealed_state,
            &transfer,
            &validators,
            digest,
            &[vec![CurrencyAddress::new(2)]
                .into_iter()
                .collect::<crate::AddressRanges>()],
        ),
        Err(PreparationError::Persistence(
            PersistenceError::TaskAdmissionClosed { .. }
        ))
    ));
    assert_eq!(sealed.load().unwrap().unwrap().generation, before_upgrade);
    assert!(matches!(
        sealed_book.admit_transition_handoff(&mut sealed_state, &transition, &authorizers, 3),
        Err(PreparationError::Persistence(
            PersistenceError::StalePreparedTasks
        ))
    ));
    assert_eq!(sealed.load().unwrap().unwrap().generation, before_upgrade);
    assert_eq!(sealed_book.claimed_currency_count(), 1);
    assert_eq!(
        sealed_book
            .collect_transition_handoff(&mut sealed_state, &extended, &authorizers, 3)
            .unwrap()
            .digest(),
        extended.digest()
    );
    assert_eq!(sealed.load().unwrap().unwrap().generation, before_upgrade);
    let collected = book
        .collect_transition_handoff(&mut state, &transition, &authorizers, 3)
        .unwrap();
    assert_ne!(collected.digest(), transition.digest());
    assert_eq!(collected.handoff.as_ref().unwrap().plans.len(), 3);
    assert!(
        collected
            .handoff
            .as_ref()
            .unwrap()
            .task_context(&local_only.task_id())
            .is_some()
    );
    let cold = StateStore::new(&receiver_base).load().unwrap().unwrap();
    assert!(
        cold.prepared_tasks[&transfer.task_id()]
            .candidate(digest)
            .unwrap()
            .is_some_and(|plan| !plan.commit_authorized)
    );
    assert_eq!(
        PreparedTaskBook::new(receiver.clone())
            .unwrap()
            .claimed_currency_count(),
        1
    );
    assert_eq!(cold.state.balance(bob), 0);
    assert!(matches!(
        cold.pending_governance.get(&collected.digest()),
        Some(crate::persistence::PendingGovernance::CollectingTransition(value))
            if value == &collected
    ));
    assert!(
        cold.pending_governance
            .values()
            .all(|value| value.target().is_none())
    );
    assert!(cold.validator_vote_locks.is_empty());
    assert!(cold.bft_local_states.is_empty());
    assert!(cold.prepared_tasks[&local_only.task_id()].commit_authorized);
    let generation = cold.generation;
    let repeated = book
        .collect_transition_handoff(&mut state, &transition, &authorizers, 4)
        .unwrap();
    assert_eq!(repeated.digest(), collected.digest());
    book.admit_transition_handoff(&mut state, &collected, &authorizers, 4)
        .unwrap();
    assert_eq!(receiver.load().unwrap().unwrap().generation, generation);
    let signer = crate::ValidatorSigner::new(crate::ValidatorId::new(2), key(7), receiver.clone());
    assert!(matches!(
        signer.sign_validator_set_transition(&collected, &validators),
        Err(crate::ValidatorSigningError::Persistence(
            PersistenceError::TransitionCollectionIncomplete { .. }
        ))
    ));
    assert!(matches!(
        signer.sign_bft_prevote(
            collected.scope(),
            1,
            crate::BftValue::Digest(collected.digest()),
            &validators,
            None,
        ),
        Err(crate::ValidatorSigningError::Persistence(
            PersistenceError::TransitionCollectionIncomplete { .. }
        ))
    ));
    assert_eq!(receiver.load().unwrap().unwrap().generation, generation);
    let runtime = crate::NodeRuntime::bind_loaded(
        "127.0.0.1:0".parse().unwrap(),
        &receiver,
        StateStore::new(&receiver_base).load().unwrap().unwrap(),
        crate::NodeRuntimeCapabilities::default().with_validator(
            crate::ValidatorRuntimeKeys::new(crate::ValidatorId::new(2), key(6), key(7)),
            crate::ValidatorRuntimeConfig::new(
                authorizers.clone(),
                crate::BftTimeoutConfig::new(
                    std::time::Duration::from_secs(1),
                    std::time::Duration::from_secs(1),
                    std::time::Duration::from_secs(1),
                ),
                || 4,
            ),
        ),
    )
    .unwrap();
    runtime.resume_governance().unwrap();
    runtime
        .start_validator_set_transition_consensus(collected.clone())
        .unwrap();
    assert!(runtime.drain_bft_consensus_events().unwrap().is_empty());
    assert_eq!(receiver.load().unwrap().unwrap().generation, generation);
    crate::prepared::tests::advance_nil_round(
        &receiver,
        &validators,
        crate::ValidatorId::new(2),
        collected.scope(),
        0,
    );
    runtime.advance_transition_collections().unwrap();
    let promoted = receiver.load().unwrap().unwrap();
    assert_eq!(promoted.generation, generation + 2);
    assert!(
        matches!(promoted.pending_governance.get(&collected.digest()),
        Some(crate::persistence::PendingGovernance::Transition(value)) if value == &collected)
    );
    assert!(promoted.validator_vote_locks.is_empty());
    assert_eq!(
        promoted.bft_local_states[&(crate::ValidatorId::new(2), collected.scope())].round(),
        1
    );
    let generation = promoted.generation;
    drop(runtime);

    let additional = crate::test_helpers::sign(
        LegalTaskPayload::new(
            TaskId::parse("handoff-third-contributor").unwrap(),
            1,
            None,
            vec![Operation::RegisterAccount {
                account: crate::test_helpers::account(175),
            }],
        ),
        &key(9),
    )
    .unwrap()
    .verify(&authorizers)
    .unwrap();
    assert!(matches!(
        book.prepare(&mut state, &additional, 4, &validators),
        Err(PreparationError::Persistence(
            PersistenceError::TaskAdmissionClosed { .. }
        ))
    ));
    assert_eq!(receiver.load().unwrap().unwrap().generation, generation);
    let mut fresh_state = fresh.load().unwrap().unwrap().state;
    PreparedTaskBook::new(fresh.clone())
        .unwrap()
        .prepare(&mut fresh_state, &additional, 4, &validators)
        .unwrap();
    let contribution = transition
        .clone()
        .with_handoff(
            crate::persistence::TaskHandoff::capture(&fresh.load().unwrap().unwrap()).unwrap(),
        )
        .unwrap();
    let extended = book
        .collect_transition_handoff(&mut state, &contribution, &authorizers, 4)
        .unwrap();
    assert_ne!(extended.digest(), collected.digest());
    let extended_cold = StateStore::new(&receiver_base).load().unwrap().unwrap();
    assert_eq!(extended_cold.pending_governance.len(), 2);
    assert!(
        extended_cold
            .pending_governance
            .contains_key(&collected.digest())
    );
    assert!(
        extended_cold
            .pending_governance
            .contains_key(&extended.digest())
    );
    assert!(!extended_cold.prepared_tasks[&additional.task_id()].commit_authorized);
    assert_eq!(extended.handoff.as_ref().unwrap().plans.len(), 4);
    // A different arrival order yields identical canonical bytes and root.
    // Remove the unrelated expired fixture before comparing the two unions.
    let expected = provider.load_prepared_tasks().unwrap();
    let mut without_expired = expected.clone();
    without_expired.remove(&expired_new.task_id());
    provider
        .replace_prepared_tasks(&expected, &without_expired)
        .unwrap();
    let reverse = PreparedTaskBook::new(provider.clone())
        .unwrap()
        .collect_transition_handoff(&mut provider_state, &extended, &authorizers, 4)
        .unwrap();
    assert_eq!(reverse.digest(), extended.digest());
    assert_eq!(
        reverse.handoff.as_ref().unwrap().encode().unwrap(),
        extended.handoff.as_ref().unwrap().encode().unwrap()
    );
    let provider_cold = StateStore::new(&provider_base).load().unwrap().unwrap();
    assert!(!provider_cold.prepared_tasks[&local_only.task_id()].commit_authorized);
    assert!(matches!(
        provider_cold.pending_governance.get(&extended.digest()),
        Some(crate::persistence::PendingGovernance::CollectingTransition(
            _
        ))
    ));
    for (store, base) in [
        (provider, provider_base),
        (receiver, receiver_base),
        (fresh, fresh_base),
        (sealed, sealed_base),
    ] {
        store.remove_files().unwrap();
        let _ = std::fs::remove_file(base.with_extension("lock"));
    }
}
