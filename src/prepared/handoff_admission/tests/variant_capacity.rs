use super::*;

#[test]
fn collection_persists_every_frozen_variant_beyond_the_message_window() {
    let validators = validator_set();
    let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
    let alice = crate::test_helpers::account(225);
    let bob = crate::test_helpers::account(226);
    let source = crate::PaymentAddress::from_bytes([225; 32]);
    let destination = crate::PaymentAddress::from_bytes([226; 32]);
    let count = crate::runtime_bft_consensus::MAX_PENDING_MESSAGES_PER_SCOPE + 1;
    let mut initial = SecondState::genesis([alice, bob], count as u64 + 1);
    for (address, account) in [(source, alice), (destination, bob)] {
        initial.business.payment_addresses.insert(
            address,
            crate::payment::PaymentAddressRecord {
                account,
                status: crate::PaymentAddressStatus::Active,
            },
        );
    }
    for value in 1..=count as u64 {
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
    let task = crate::test_helpers::sign(
        LegalTaskPayload::new(
            TaskId::parse("handoff-variant-capacity").unwrap(),
            1,
            None,
            vec![Operation::Transfer {
                source,
                destination,
                amount: 1,
            }],
        ),
        &key(9),
    )
    .unwrap()
    .verify(&authorizers)
    .unwrap();
    let (store, base) = temp_store();
    store.initialize(&initial, &validators).unwrap();
    let mut state = initial.clone();
    let mut book = PreparedTaskBook::new(store.clone()).unwrap();
    book.prepare(&mut state, &task, 1, &validators).unwrap();
    let snapshot = store.load().unwrap().unwrap();
    let primary = snapshot.prepared_tasks[&task.task_id()].clone();
    let mut handoff = crate::persistence::TaskHandoff::capture(&snapshot).unwrap();
    for value in 2..=count as u64 {
        let mut candidate = primary.clone();
        let crate::prepared_plan::PreparedOperation::Transfer { currencies, .. } =
            &mut candidate.operations[0]
        else {
            panic!("transfer fixture")
        };
        *currencies = vec![CurrencyAddress::new(value)];
        handoff.insert(candidate).unwrap();
    }
    let handoff = crate::persistence::TaskHandoff::decode(&handoff.encode().unwrap()).unwrap();
    let transition = ValidatorSetTransition::new(
        1,
        &validators,
        &snapshot.validator_registry,
        ValidatorSet::new(2, validators.credentials().cloned()).unwrap(),
        vec![],
        vec![],
        count as u64 + 1,
    )
    .unwrap()
    .with_handoff(handoff)
    .unwrap();
    let collected = book
        .collect_transition_handoff(&mut state, &transition, &authorizers, 1)
        .unwrap();
    let cold = StateStore::new(&base).load().unwrap().unwrap();
    let plan = &cold.prepared_tasks[&task.task_id()];
    assert_eq!(plan.variants.len() + 1, count);
    assert_eq!(
        plan.owned_candidate_digests().unwrap(),
        vec![primary.plan_digest().unwrap()]
    );
    assert_eq!(book.claimed_currency_count(), 1);
    assert_eq!(cold.state.balance(alice), count as u64);
    assert_eq!(cold.state.balance(bob), 0);
    assert_eq!(collected.handoff.as_ref().unwrap().plans.len(), count);
    collected.handoff.as_ref().unwrap().covers(&cold).unwrap();
    assert!(cold.validator_vote_locks.is_empty());
    assert!(cold.bft_local_states.is_empty());
    let generation = cold.generation;
    let repeated = book
        .collect_transition_handoff(&mut state, &collected, &authorizers, 1)
        .unwrap();
    assert_eq!(repeated.digest(), collected.digest());
    assert_eq!(store.load().unwrap().unwrap().generation, generation);
    // A certified cut may retain the first candidate and retire the second's
    // unvoted ownership, without erasing its exact frozen witness.
    let second = collected.handoff.as_ref().unwrap().plans.values().find(|candidate| {
        matches!(&candidate.operations[0], crate::prepared_plan::PreparedOperation::Transfer { currencies, .. }
            if currencies == &[CurrencyAddress::new(2)])
    }).unwrap().clone();
    book.admit_frozen_variant(
        &mut state,
        &task,
        &validators,
        second.plan_digest().unwrap(),
        &[vec![CurrencyAddress::new(2)]],
    )
    .unwrap();
    assert_eq!(book.claimed_currency_count(), 2);
    let snapshot = store.load().unwrap().unwrap();
    let mut selected = snapshot.prepared_tasks.clone();
    selected
        .get_mut(&task.task_id())
        .unwrap()
        .select_candidate(second.plan_digest().unwrap())
        .unwrap();
    store
        .save_with_prepared(
            &snapshot.state,
            &snapshot.state,
            &validators,
            &snapshot.prepared_tasks,
            &selected,
        )
        .unwrap();
    let chosen = crate::BftStatement::new(
        1,
        crate::ConsensusScope::PreparedTask(task.task_id()),
        0,
        crate::BftPhase::Precommit,
        crate::BftValue::Digest(primary.plan_digest().unwrap()),
    );
    let qc = crate::BftQuorumCertificate::new(
        chosen.clone(),
        (1..=3)
            .map(|id| {
                crate::BftVote::sign_unchecked(
                    &chosen,
                    crate::ValidatorId::new(id),
                    &key((id * 3 + 1) as u8),
                )
            })
            .collect(),
        &validators,
    )
    .unwrap();
    store
        .accept_bft_precommit_qc(crate::ValidatorId::new(4), &qc, &validators)
        .unwrap();
    let mut subset_snapshot = store.load().unwrap().unwrap();
    subset_snapshot.prepared_tasks =
        std::collections::BTreeMap::from([(task.task_id(), primary.clone())]);
    let subset = crate::persistence::TaskHandoff::capture(&subset_snapshot).unwrap();
    let cut = collected.clone().with_handoff(subset).unwrap();
    let statement = cut.finality_statement();
    let certified = crate::CertifiedValidatorSetTransition::new(
        cut,
        (1..=3)
            .map(|id| {
                crate::ValidatorVote::sign_unchecked(
                    &statement,
                    crate::ValidatorId::new(id),
                    &key((id * 3 + 1) as u8),
                )
            })
            .collect(),
        &validators,
    )
    .unwrap();
    store.activate_validator_set_transition(&certified).unwrap();
    let cold = StateStore::new(&base).load().unwrap().unwrap();
    let plan = &cold.prepared_tasks[&task.task_id()];
    assert!(plan.commit_authorized);
    assert_eq!(plan.plan_digest().unwrap(), primary.plan_digest().unwrap());
    assert_eq!(plan.variants.len() + 1, count);
    assert_eq!(
        plan.owned_candidate_digests().unwrap(),
        vec![primary.plan_digest().unwrap()]
    );
    assert!(
        !plan
            .candidate(second.plan_digest().unwrap())
            .unwrap()
            .unwrap()
            .commit_authorized
    );
    let signer = crate::ValidatorSigner::new(crate::ValidatorId::new(4), key(13), store.clone());
    let generation = cold.generation;
    assert!(
        signer
            .sign_bft_prevote(
                crate::ConsensusScope::PreparedTask(task.task_id()),
                0,
                crate::BftValue::Digest(second.plan_digest().unwrap()),
                &validators,
                None
            )
            .is_err()
    );
    assert_eq!(store.load().unwrap().unwrap().generation, generation);
    assert_eq!(
        PreparedTaskBook::new(store.clone())
            .unwrap()
            .claimed_currency_count(),
        1
    );
    assert!(
        store
            .bft_finality_ready(
                crate::ValidatorId::new(4),
                &crate::ConsensusScope::PreparedTask(task.task_id()),
                primary.plan_digest().unwrap()
            )
            .unwrap()
    );
    store.remove_files().unwrap();
    let _ = std::fs::remove_file(base.with_extension("lock"));
}
