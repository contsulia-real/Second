use super::*;
use crate::prepared::tests::{key, temp_store, validator_set};
use crate::{AuthorizerSet, LegalTaskPayload, ValidatorId, ValidatorVote};

#[tokio::test]
async fn certified_resource_ring_survives_partial_evidence_and_commits_atomically_without_signing_rights()
 {
    let validators = validator_set();
    let alice = crate::test_helpers::account(121);
    let bob = crate::test_helpers::account(122);
    let source = crate::PaymentAddress::from_bytes([121; 32]);
    let destination = crate::PaymentAddress::from_bytes([122; 32]);
    let mut state = SecondState::genesis([alice, bob], 3);
    for (address, account) in [(source, alice), (destination, bob)] {
        state.business.payment_addresses.insert(
            address,
            crate::payment::PaymentAddressRecord {
                account,
                status: crate::PaymentAddressStatus::Active,
            },
        );
    }
    for number in 1..=2 {
        let address = crate::CurrencyAddress::new(number);
        state.business.currencies.insert(
            address,
            crate::currency::Currency {
                address,
                role: crate::CurrencyRole::Circulation,
                owner: Some(alice),
            },
        );
    }
    let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
    let task = |name| {
        crate::test_helpers::sign(
            LegalTaskPayload::new(
                TaskId::parse(name).unwrap(),
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
        .unwrap()
    };
    let a = task("certified-ring-a");
    let b = task("certified-ring-b");
    let (store, base) = temp_store();
    store.initialize(&state, &validators).unwrap();
    let mut book = PreparedTaskBook::new(store.clone()).unwrap();
    book.prepare(&mut state, &a, 1, &validators).unwrap();
    book.prepare(&mut state, &b, 1, &validators).unwrap();
    let mut certificates = Vec::new();
    for (task, number) in [(&a, 2), (&b, 1)] {
        let mut candidate = store
            .load_prepared_tasks()
            .unwrap()
            .remove(&task.task_id())
            .unwrap();
        let PreparedOperation::Transfer { currencies, .. } = &mut candidate.operations[0] else {
            panic!("transfer fixture")
        };
        *currencies = vec![crate::CurrencyAddress::new(number)];
        let digest = candidate.plan_digest().unwrap();
        let contention = book
            .admit_frozen_variant(
                &mut state,
                task,
                &validators,
                digest,
                &[vec![crate::CurrencyAddress::new(number)]],
            )
            .unwrap()
            .unwrap();
        book.admit_contention(&mut state, task, &validators, contention)
            .unwrap();
        let statement = FinalityStatement::new(1, 1, digest);
        certificates.push(
            FinalityCertificate::new(
                statement,
                (2..=4)
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
            .unwrap(),
        );
    }
    store
        .finalize_prepared_task(
            &a.task_id(),
            certificates[0].statement().subject_digest(),
            &certificates[0],
        )
        .unwrap();
    let cold_store = StateStore::new(&base);
    let cold = cold_store.load().unwrap().unwrap();
    assert!(!cold.prepared_tasks[&a.task_id()].commit_authorized);
    assert!(cold.prepared_tasks[&a.task_id()].has_owned_candidate());
    let generation = cold.generation;
    assert!(
        PreparedTaskBook::commit_certified_component(&cold_store, &a.task_id(), None)
            .unwrap()
            .completed
            .is_empty()
    );
    assert_eq!(cold_store.load().unwrap().unwrap().generation, generation);
    assert_eq!(
        PreparedTaskBook::new(cold_store.clone())
            .unwrap()
            .claimed_currency_count(),
        2
    );
    assert_eq!(cold.state.balance(bob), 0);
    let signer = crate::ValidatorSigner::new(ValidatorId::new(1), key(4), cold_store.clone());
    assert!(
        signer
            .sign_prepared_task(a.task_id(), &certificates[0].statement(), &validators)
            .is_err()
    );
    assert!(
        signer
            .sign_bft_prevote(
                crate::ConsensusScope::PreparedTask(a.task_id()),
                0,
                crate::BftValue::Digest(certificates[0].statement().subject_digest()),
                &validators,
                None,
            )
            .is_err()
    );
    assert_eq!(cold_store.load().unwrap().unwrap().generation, generation);
    let forged = FinalityCertificate::from_untrusted_parts(
        certificates[1].statement(),
        (2..=4)
            .map(|id| {
                ValidatorVote::sign_unchecked(
                    &certificates[1].statement(),
                    ValidatorId::new(id),
                    &key(99),
                )
            })
            .collect(),
    );
    assert!(
        cold_store
            .finalize_prepared_task(&b.task_id(), forged.statement().subject_digest(), &forged)
            .is_err()
    );
    assert_eq!(cold_store.load().unwrap().unwrap().generation, generation);
    let fork = || {
        let (forked, path) = temp_store();
        forked.initialize(&cold.state, &validators).unwrap();
        forked
            .replace_prepared_tasks(&BTreeMap::new(), &cold.prepared_tasks)
            .unwrap();
        (forked, path)
    };
    // A witness has no local payment establishment rights. Its certified
    // execution must use the same current address/account checks as preparation.
    for status in [
        crate::PaymentAddressStatus::Active,
        crate::PaymentAddressStatus::Retiring,
    ] {
        let (witnessed, witnessed_base) = temp_store();
        let mut witness_state = SecondState::genesis([alice, bob], 3);
        witness_state.business = cold.state.business.clone();
        witness_state
            .business
            .payment_addresses
            .get_mut(&source)
            .unwrap()
            .status = status;
        witness_state.bind_task(&a).unwrap();
        witnessed.initialize(&witness_state, &validators).unwrap();
        let mut plan = cold.prepared_tasks[&a.task_id()].clone();
        plan.variants.clear();
        witnessed
            .replace_prepared_tasks(&BTreeMap::new(), &BTreeMap::from([(a.task_id(), plan)]))
            .unwrap();
        let before = witnessed.load().unwrap().unwrap().generation;
        let result = PreparedTaskBook::recover_finalized_from_store(&witnessed);
        let after = witnessed.load().unwrap().unwrap();
        if status == crate::PaymentAddressStatus::Active {
            result.unwrap();
            assert_eq!(after.state.task_succeeded(a.task_id()), Some(true));
            assert_eq!(after.state.balance(bob), 1);
            assert_eq!(after.generation, before + 1);
        } else {
            assert!(
                matches!(result, Err(PreparationError::Execution(crate::ExecutionError::PaymentAddressUnavailable(address))) if address == source)
            );
            assert_eq!(after.generation, before);
            assert_eq!(after.state.balance(bob), 0);
        }
        witnessed.remove_files().unwrap();
        let _ = std::fs::remove_file(witnessed_base.with_extension("lock"));
    }
    let (contradictory, contradictory_base) = fork();
    let conflicting_statement = FinalityStatement::new(
        1,
        1,
        cold.prepared_tasks[&b.task_id()].plan_digest().unwrap(),
    );
    let conflicting = FinalityCertificate::new(
        conflicting_statement,
        (2..=4)
            .map(|id| {
                ValidatorVote::sign_unchecked(
                    &conflicting_statement,
                    ValidatorId::new(id),
                    &key((id * 3 + 1) as u8),
                )
            })
            .collect(),
        &validators,
    )
    .unwrap();
    contradictory
        .finalize_prepared_task(
            &b.task_id(),
            conflicting_statement.subject_digest(),
            &conflicting,
        )
        .unwrap();
    let generation_before = contradictory.load().unwrap().unwrap().generation;
    assert!(PreparedTaskBook::recover_finalized_from_store(&contradictory).is_err());
    let mut contradictory_state = contradictory.load().unwrap().unwrap().state;
    assert!(
        PreparedTaskBook::new(contradictory.clone())
            .unwrap()
            .commit(&mut contradictory_state, b.task_id(), &conflicting,)
            .is_err()
    );
    assert_eq!(
        contradictory.load().unwrap().unwrap().generation,
        generation_before
    );
    assert_eq!(contradictory.load().unwrap().unwrap().state.balance(bob), 0);
    let (online, online_base) = fork();
    let bind = |store: &StateStore, id| {
        crate::NodeRuntime::bind_loaded(
            "127.0.0.1:0".parse().unwrap(),
            store,
            store.load().unwrap().unwrap(),
            crate::NodeRuntimeCapabilities::default().with_validator(
                crate::ValidatorRuntimeKeys::new(
                    ValidatorId::new(id),
                    key((id * 3) as u8),
                    key((id * 3 + 1) as u8),
                ),
                crate::ValidatorRuntimeConfig::new(
                    authorizers.clone(),
                    crate::BftTimeoutConfig::new(
                        std::time::Duration::from_secs(1),
                        std::time::Duration::from_secs(1),
                        std::time::Duration::from_secs(1),
                    ),
                    || 1,
                ),
            ),
        )
        .unwrap()
    };
    let runtime = std::sync::Arc::new(bind(&online, 1));
    runtime.start_durable_prepared_consensus().unwrap();
    assert_eq!(online.load().unwrap().unwrap().state.balance(bob), 0);
    let (provider_store, provider_base) = fork();
    provider_store
        .finalize_prepared_task(
            &b.task_id(),
            certificates[1].statement().subject_digest(),
            &certificates[1],
        )
        .unwrap();
    let provider = std::sync::Arc::new(bind(&provider_store, 2));
    let records = [
        runtime.local_peer_record().unwrap().clone(),
        provider.local_peer_record().unwrap().clone(),
    ];
    let workers = [
        std::sync::Arc::clone(&runtime),
        std::sync::Arc::clone(&provider),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, node)| {
        let record = records[1 - index].clone();
        tokio::spawn(async move {
            node.run(&[record]).await.unwrap();
        })
    })
    .collect::<Vec<_>>();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !provider
            .connected_validator_ids()
            .contains(&ValidatorId::new(1))
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            provider
                .validator_bft
                .as_ref()
                .unwrap()
                .broadcast(&crate::BftNetworkMessage::FinalityCertificate {
                    scope: crate::ConsensusScope::PreparedTask(b.task_id()),
                    certificate: certificates[1].clone(),
                })
                .is_empty()
        );
        while online
            .load()
            .unwrap()
            .unwrap()
            .state
            .task_succeeded(b.task_id())
            != Some(true)
            || !runtime
                .validator_bft
                .as_ref()
                .unwrap()
                .consensus()
                .completed_relay_authorities()
                .iter()
                .any(|(scope, _)| scope == &crate::ConsensusScope::PreparedTask(b.task_id()))
        {
            assert!(workers.iter().all(|worker| !worker.is_finished()));
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("authenticated QUIC finality must recover the follower's retained resource ring");
    for worker in &workers {
        worker.abort();
    }
    for worker in workers {
        let _ = worker.await;
    }
    let output = runtime
        .validator_bft
        .as_ref()
        .unwrap()
        .consensus()
        .drive(Vec::new(), tokio::time::Instant::now());
    let committed_online = StateStore::new(&online_base).load().unwrap().unwrap();
    assert_eq!(
        committed_online.state.task_succeeded(a.task_id()),
        Some(true)
    );
    assert_eq!(
        committed_online.state.task_succeeded(b.task_id()),
        Some(true)
    );
    assert_eq!(committed_online.state.balance(bob), 2);
    let statement = crate::BftStatement::new(
        1,
        crate::ConsensusScope::PreparedTask(b.task_id()),
        0,
        crate::BftPhase::Prevote,
        crate::BftValue::Nil,
    );
    let relay = runtime.validator_bft.as_ref().unwrap().consensus().drive(
        vec![crate::runtime_bft::InboundBftMessage {
            validator_id: ValidatorId::new(2),
            message: crate::BftNetworkMessage::Vote {
                statement: statement.clone(),
                vote: crate::BftVote::sign_unchecked(&statement, ValidatorId::new(2), &key(7)),
            },
        }],
        tokio::time::Instant::now() + std::time::Duration::from_secs(2),
    );
    assert!(relay.outbound.iter().any(|message| matches!(message,
        crate::BftNetworkMessage::FinalityCertificate { certificate, .. }
            if certificate.statement() == certificates[1].statement())));
    assert!(!output.outbound.iter().any(|message| matches!(
        message,
        crate::BftNetworkMessage::Proposal { .. } | crate::BftNetworkMessage::Vote { .. }
    )));
    drop(runtime);
    drop(provider);
    for (forked, path) in [
        (contradictory, contradictory_base),
        (online, online_base),
        (provider_store, provider_base),
    ] {
        forked.remove_files().unwrap();
        let _ = std::fs::remove_file(path.with_extension("lock"));
    }
    cold_store
        .finalize_prepared_task(
            &b.task_id(),
            certificates[1].statement().subject_digest(),
            &certificates[1],
        )
        .unwrap();
    let generation = cold_store.load().unwrap().unwrap().generation;
    // Cold recovery executes both exact certified choices in one business write.
    let restarted = StateStore::new(&base);
    PreparedTaskBook::recover_finalized_from_store(&restarted).unwrap();
    let committed = StateStore::new(&base).load().unwrap().unwrap();
    assert_eq!(committed.generation, generation + 1);
    assert_eq!(committed.state.task_succeeded(a.task_id()), Some(true));
    assert_eq!(committed.state.task_succeeded(b.task_id()), Some(true));
    assert_eq!(committed.state.balance(bob), 2);
    assert!(committed.prepared_tasks.is_empty());
    assert_eq!(
        committed.task_receipts[&a.task_id()]
            .certificate()
            .unwrap()
            .statement(),
        certificates[0].statement()
    );
    assert_eq!(
        committed.task_receipts[&b.task_id()]
            .certificate()
            .unwrap()
            .statement(),
        certificates[1].statement()
    );
    PreparedTaskBook::recover_finalized_from_store(&restarted).unwrap();
    assert_eq!(
        restarted.load().unwrap().unwrap().generation,
        committed.generation
    );
    store.remove_files().unwrap();
    let _ = std::fs::remove_file(base.with_extension("lock"));
}
