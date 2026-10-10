//! A certified old execution remains recoverable after an address starts retirement.
use super::*;

#[tokio::test]
async fn inherited_transfer_finality_recovers_missing_execution_on_retiring_address() {
    for validator in [1, 5] {
        recover_inherited_transfer(validator).await;
    }
}

async fn recover_inherited_transfer(validator: u64) {
    let origin = validator_set();
    let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
    let alice = crate::test_helpers::account(211);
    let bob = crate::test_helpers::account(212);
    let source = PaymentAddress::from_bytes([211; 32]);
    let destination = PaymentAddress::from_bytes([212; 32]);
    let mut initial = SecondState::genesis([alice, bob], 2);
    for (address, account) in [(source, alice), (destination, bob)] {
        initial.business.payment_addresses.insert(
            address,
            crate::payment::PaymentAddressRecord {
                account,
                status: PaymentAddressStatus::Active,
            },
        );
    }
    let currency = CurrencyAddress::new(1);
    initial.business.currencies.insert(
        currency,
        crate::currency::Currency {
            address: currency,
            role: CurrencyRole::Circulation,
            owner: Some(alice),
        },
    );
    let sign = |name: &str, operations| {
        crate::test_helpers::sign(
            LegalTaskPayload::new(TaskId::parse(name).unwrap(), 1, None, operations),
            &key(9),
        )
        .unwrap()
        .verify(&authorizers)
        .unwrap()
    };
    let task = sign(
        "inherited-retiring-transfer",
        vec![Operation::Transfer {
            source,
            destination,
            amount: 1,
        }],
    );
    let (provider, provider_base) = temp_store();
    provider.initialize(&initial, &origin).unwrap();
    let mut book = PreparedTaskBook::new(provider.clone()).unwrap();
    book.prepare(&mut initial.clone(), &task, 1, &origin)
        .unwrap();
    let statement = book.prepared_finality_statement(task.task_id()).unwrap();
    let votes = |statement: FinalityStatement| {
        (1..=3)
            .map(|id| {
                ValidatorVote::sign_unchecked(
                    &statement,
                    ValidatorId::new(id),
                    &key((id * 3 + 1) as u8),
                )
            })
            .collect()
    };
    let finality = FinalityCertificate::new(statement, votes(statement), &origin).unwrap();
    let trusted = provider.load().unwrap().unwrap();
    let joining = ValidatorCredential::new(
        ValidatorId::new(5),
        key(15).verifying_key().to_bytes(),
        key(16).verifying_key().to_bytes(),
        key(17).verifying_key().to_bytes(),
    )
    .unwrap();
    let admission =
        ValidatorAdmissionRequest::sign(1, joining.clone(), &key(15), &key(16), &key(17))
            .unwrap()
            .verify()
            .unwrap();
    let active = ValidatorSet::new(
        2,
        origin
            .credentials()
            .filter(|credential| credential.id() != ValidatorId::new(4))
            .cloned()
            .chain(std::iter::once(joining)),
    )
    .unwrap();
    let transition = provider
        .prepare_validator_set_transition(
            ValidatorSetTransition::new(
                1,
                &origin,
                &trusted.validator_registry,
                active.clone(),
                vec![admission],
                vec![],
                2,
            )
            .unwrap(),
        )
        .unwrap();
    let body = transition.handoff.as_ref().unwrap().encode().unwrap();
    let certified = CertifiedValidatorSetTransition::new(
        transition.clone(),
        votes(transition.finality_statement()),
        &origin,
    )
    .unwrap();
    let (target, base) = temp_store();
    target
        .install_validator_handoff_baseline(
            &ValidatorSetTransitionProof::from_certified(&certified),
            &body,
            &trusted,
            &authorizers,
        )
        .unwrap();
    let mut current = target.load().unwrap().unwrap().state;
    let retire = sign(
        "inherited-retiring-address",
        vec![Operation::RetirePaymentAddress {
            address: destination,
        }],
    );
    let mut book = PreparedTaskBook::new(target.clone()).unwrap();
    book.prepare(&mut current, &retire, 1, &active).unwrap();
    let statement = book.prepared_finality_statement(retire.task_id()).unwrap();
    book.commit(
        &mut current,
        retire.task_id(),
        &FinalityCertificate::new(statement, votes(statement), &active).unwrap(),
    )
    .unwrap();
    let finalize = sign(
        "inherited-finalize-address",
        vec![Operation::FinalizePaymentAddressRetirement {
            address: destination,
        }],
    );
    assert!(
        matches!(
            book.prepare(&mut current, &finalize, 1, &active),
            Err(PreparationError::CertifiedResourceFence { blockers }) if blockers == vec![task.task_id()]
        ),
        "retirement finalization ignored the inherited execution obligation"
    );
    let before = target.load().unwrap().unwrap();
    assert!(before.state.prerequisite.payment_executions.is_empty());
    assert_eq!(
        before.state.business.payment_addresses[&destination].status,
        PaymentAddressStatus::Retiring
    );
    let node = NodeRuntime::bind_loaded(
        "127.0.0.1:0".parse().unwrap(),
        &target,
        before.clone(),
        NodeRuntimeCapabilities::default().with_validator(
            ValidatorRuntimeKeys::new(
                ValidatorId::new(validator),
                key((validator * 3) as u8),
                key((validator * 3 + 1) as u8),
            ),
            ValidatorRuntimeConfig::new(
                authorizers.clone(),
                BftTimeoutConfig::new(
                    Duration::from_secs(1),
                    Duration::from_secs(1),
                    Duration::from_secs(1),
                ),
                || 1,
            ),
        ),
    )
    .unwrap();
    let invalid = FinalityCertificate::from_untrusted_parts(
        finality.statement(),
        vec![ValidatorVote::from_untrusted_parts(
            ValidatorId::new(1),
            [0; 64],
        )],
    );
    assert!(
        node.install_handoff_terminal(&ConsensusScope::PreparedTask(task.task_id()), &invalid)
            .is_err()
    );
    assert_eq!(
        target.load().unwrap().unwrap().generation,
        before.generation
    );
    provider
        .activate_validator_set_transition(&certified)
        .unwrap();
    provider
        .finalize_prepared_task(
            &task.task_id(),
            finality.statement().subject_digest(),
            &finality,
        )
        .unwrap();
    PreparedTaskBook::recover_finalized_from_store(&provider).unwrap();
    let sender = std::sync::Arc::new(
        NodeRuntime::bind_loaded(
            "127.0.0.1:0".parse().unwrap(),
            &provider,
            provider.load().unwrap().unwrap(),
            NodeRuntimeCapabilities::default().with_validator(
                ValidatorRuntimeKeys::new(ValidatorId::new(2), key(6), key(7)),
                ValidatorRuntimeConfig::new(
                    authorizers,
                    BftTimeoutConfig::new(
                        Duration::from_secs(1),
                        Duration::from_secs(1),
                        Duration::from_secs(1),
                    ),
                    || 1,
                ),
            ),
        )
        .unwrap(),
    );
    let node = std::sync::Arc::new(node);
    let mut workers = Vec::new();
    for running in [sender.clone(), node.clone()] {
        workers.push(tokio::spawn(async move { running.run(&[]).await }));
    }
    sender
        .dial_validator_bft(node.local_peer_record().unwrap())
        .await
        .unwrap();
    assert!(
        sender
            .validator_bft
            .as_ref()
            .unwrap()
            .broadcast(&BftNetworkMessage::FinalityCertificate {
                scope: ConsensusScope::PreparedTask(task.task_id()),
                certificate: finality,
            })
            .is_empty()
    );
    tokio::time::timeout(Duration::from_secs(3), async {
        while target
            .load()
            .unwrap()
            .unwrap()
            .state
            .task_succeeded(task.task_id())
            != Some(true)
        {
            assert!(workers.iter().all(|worker| !worker.is_finished()));
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("authenticated historical finality did not recover the old execution");
    let cold = StateStore::new(&base).load().unwrap().unwrap();
    assert_eq!(
        cold.state.task_succeeded(task.task_id()),
        Some(true),
        "certified old transfer remained stuck after retirement started"
    );
    assert_eq!(
        cold.state.business.currencies.get(&currency).unwrap().owner,
        Some(bob)
    );
    assert_eq!(
        cold.state.business.payment_addresses[&destination].status,
        PaymentAddressStatus::Retiring
    );
    assert!(cold.state.prerequisite.payment_executions.is_empty());
    assert!(cold.validator_vote_locks.is_empty());
    assert!(!cold.validator_safety_ready);
    for worker in &workers {
        worker.abort();
    }
    for worker in workers {
        let _ = worker.await;
    }
    drop(sender);
    drop(node);
    let mut current = cold.state;
    let mut book = PreparedTaskBook::new(target.clone()).unwrap();
    book.prepare(&mut current, &finalize, 1, &active).unwrap();
    let statement = book
        .prepared_finality_statement(finalize.task_id())
        .unwrap();
    book.commit(
        &mut current,
        finalize.task_id(),
        &FinalityCertificate::new(statement, votes(statement), &active).unwrap(),
    )
    .unwrap();
    assert_eq!(
        target
            .load()
            .unwrap()
            .unwrap()
            .state
            .business
            .payment_addresses[&destination]
            .status,
        PaymentAddressStatus::Retired
    );
    target.remove_files().unwrap();
    provider.remove_files().unwrap();
    for base in [base, provider_base] {
        for suffix in ["transport", "transport.lock", "peers"] {
            let _ = std::fs::remove_file(base.with_extension(suffix));
        }
    }
}
