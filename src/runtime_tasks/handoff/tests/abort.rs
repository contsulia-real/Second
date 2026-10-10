//! A late original-quorum Abort must wake current resource contenders.
use super::*;

#[tokio::test]
async fn inherited_abort_retries_waiting_business_with_or_without_local_body() {
    for (local_body, variants) in [(false, false), (true, false), (false, true), (true, true)] {
        let origin = validator_set();
        let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
        let account = crate::test_helpers::account(211);
        let recipient = crate::test_helpers::account(212);
        let from = PaymentAddress::from_bytes([211; 32]);
        let to = PaymentAddress::from_bytes([212; 32]);
        let request = |name| {
            crate::test_helpers::sign(
                LegalTaskPayload::new(
                    TaskId::parse(name).unwrap(),
                    1,
                    None,
                    vec![if variants {
                        Operation::Transfer {
                            source: from,
                            destination: to,
                            amount: 1,
                        }
                    } else {
                        Operation::RegisterAccount { account }
                    }],
                ),
                &key(9),
            )
            .unwrap()
            .verify(&authorizers)
            .unwrap()
        };
        let holder = request("inherited-abort-holder");
        let waiting = request("aaa-inherited-abort-waiter");
        let (source, source_base) = temp_store();
        let mut state = if variants {
            SecondState::genesis([account, recipient], 3)
        } else {
            SecondState::genesis([], 1)
        };
        if variants {
            for (address, owner) in [(from, account), (to, recipient)] {
                state.business.payment_addresses.insert(
                    address,
                    crate::payment::PaymentAddressRecord {
                        account: owner,
                        status: PaymentAddressStatus::Active,
                    },
                );
            }
            for number in 1..=2 {
                let address = CurrencyAddress::new(number);
                state.business.currencies.insert(
                    address,
                    crate::currency::Currency {
                        address,
                        role: CurrencyRole::Circulation,
                        owner: Some(account),
                    },
                );
            }
        }
        let initial = state.clone();
        source.initialize(&state, &origin).unwrap();
        PreparedTaskBook::new(source.clone())
            .unwrap()
            .prepare(&mut state, &holder, 1, &origin)
            .unwrap();
        if variants {
            let mut other =
                source.load().unwrap().unwrap().prepared_tasks[&holder.task_id()].clone();
            let crate::prepared_plan::PreparedOperation::Transfer { currencies, .. } =
                &mut other.operations[0]
            else {
                panic!("transfer fixture")
            };
            *currencies = vec![CurrencyAddress::new(2)]
                .into_iter()
                .collect::<crate::AddressRanges>();
            PreparedTaskBook::new(source.clone())
                .unwrap()
                .admit_frozen_variant(
                    &mut state,
                    &holder,
                    &origin,
                    other.plan_digest().unwrap(),
                    &[vec![CurrencyAddress::new(2)]
                        .into_iter()
                        .collect::<crate::AddressRanges>()],
                )
                .unwrap();
        }
        let trusted = source.load().unwrap().unwrap();
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
        let active =
            ValidatorSet::new(2, origin.credentials().take(3).cloned().chain([joining])).unwrap();
        let transition = source
            .prepare_validator_set_transition(
                ValidatorSetTransition::new(
                    1,
                    &origin,
                    &trusted.validator_registry,
                    active.clone(),
                    vec![admission],
                    vec![],
                    state.next_currency_address(),
                )
                .unwrap(),
            )
            .unwrap();
        let votes = |statement| {
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
        let certified = CertifiedValidatorSetTransition::new(
            transition.clone(),
            votes(transition.finality_statement()),
            &origin,
        )
        .unwrap();
        let proof = ValidatorSetTransitionProof::from_certified(&certified);
        let body = transition.handoff.as_ref().unwrap().encode().unwrap();
        let handoff = transition.handoff.as_ref().unwrap();
        let first = handoff.plans.values().next().unwrap();
        let last = handoff.plans.values().next_back().unwrap();
        let selections = |plan: &crate::prepared_plan::PreparedTask| {
            if let crate::prepared_plan::PreparedOperation::Transfer { currencies, .. } =
                &plan.operations[0]
            {
                vec![currencies.clone()]
            } else {
                vec![]
            }
        };
        let (target, target_base) = temp_store();
        target
            .install_validator_handoff_baseline(&proof, &body, &trusted, &authorizers)
            .unwrap();
        if local_body {
            let mut current = target.load().unwrap().unwrap().state;
            PreparedTaskBook::new(target.clone())
                .unwrap()
                .prepare_expected_plan(
                    &mut current,
                    &holder,
                    1,
                    &origin,
                    first.plan_digest().unwrap(),
                    &selections(first),
                )
                .unwrap();
        }
        let node = NodeRuntime::bind_loaded(
            "127.0.0.1:0".parse().unwrap(),
            &target,
            target.load().unwrap().unwrap(),
            NodeRuntimeCapabilities::default().with_validator(
                ValidatorRuntimeKeys::new(ValidatorId::new(5), key(15), key(16)),
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
        .unwrap();
        node.start_durable_prepared_consensus().unwrap();
        if variants {
            // Freeze the waiter on the last inherited candidate, not the first.
            let (provider, provider_base) = temp_store();
            let mut trial = initial;
            provider.initialize(&trial, &active).unwrap();
            PreparedTaskBook::new(provider.clone())
                .unwrap()
                .prepare(&mut trial, &waiting, 1, &active)
                .unwrap();
            let mut candidate =
                provider.load().unwrap().unwrap().prepared_tasks[&waiting.task_id()].clone();
            let crate::prepared_plan::PreparedOperation::Transfer { currencies, .. } =
                &mut candidate.operations[0]
            else {
                panic!("transfer fixture")
            };
            *currencies = selections(last).remove(0);
            node.install_fetched_prepared_task(
                2,
                &ConsensusScope::PreparedTask(waiting.task_id()),
                candidate.plan_digest().unwrap(),
                &candidate.encode_source().unwrap(),
            )
            .unwrap();
            provider.remove_files().unwrap();
            let _ = std::fs::remove_file(provider_base.with_extension("lock"));
        } else {
            assert_eq!(
                node.submit_legal_task(waiting.signed_task().clone())
                    .unwrap(),
                LegalTaskSubmissionOutcome::AlreadyPending
            );
        }
        let blocked = target.load().unwrap().unwrap();
        assert!(!blocked.prepared_tasks[&waiting.task_id()].commit_authorized);
        assert!(!blocked.prepared_tasks[&waiting.task_id()].conflict_abort);
        let statement = source.prepared_abort_statement(&holder.task_id()).unwrap();
        let abort = FinalityCertificate::new(statement, votes(statement), &origin).unwrap();
        let deliver = |certificate| crate::runtime_bft::InboundBftMessage {
            validator_id: ValidatorId::new(1),
            message: BftNetworkMessage::FinalityCertificate {
                scope: ConsensusScope::PreparedTask(holder.task_id()),
                certificate,
            },
        };
        let invalid = FinalityCertificate::from_untrusted_parts(
            statement,
            vec![ValidatorVote::from_untrusted_parts(
                ValidatorId::new(1),
                [0; 64],
            )],
        );
        assert!(
            node.process_prepared_task_sync(vec![deliver(invalid)])
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            target.load().unwrap().unwrap().generation,
            blocked.generation
        );
        assert!(
            node.process_prepared_task_sync(vec![deliver(abort.clone())])
                .unwrap()
                .is_empty()
        );
        let ready = StateStore::new(&target_base).load().unwrap().unwrap();
        assert!(ready.state.task_cancelled(holder.task_id()));
        assert!(
            ready.prepared_tasks[&waiting.task_id()].commit_authorized,
            "original-quorum Abort released the fence but did not retry the waiter; local_body={local_body}"
        );
        if variants {
            assert_eq!(ready.state.balance(recipient), 0);
        } else {
            assert!(!ready.state.has_account(account));
        }
        assert!(!ready.validator_safety_ready);
        assert!(ready.validator_vote_locks.is_empty());
        let current_statement = PreparedTaskBook::new(target.clone())
            .unwrap()
            .prepared_finality_statement(waiting.task_id())
            .unwrap();
        let current_certificate =
            FinalityCertificate::new(current_statement, votes(current_statement), &active).unwrap();
        let inbound = node
            .process_prepared_task_sync(vec![crate::runtime_bft::InboundBftMessage {
                validator_id: ValidatorId::new(1),
                message: BftNetworkMessage::FinalityCertificate {
                    scope: ConsensusScope::PreparedTask(waiting.task_id()),
                    certificate: current_certificate,
                },
            }])
            .unwrap();
        node.validator_bft
            .as_ref()
            .unwrap()
            .consensus()
            .drive(inbound, tokio::time::Instant::now());
        let committed = StateStore::new(&target_base).load().unwrap().unwrap();
        assert_eq!(
            committed.state.task_succeeded(waiting.task_id()),
            Some(true)
        );
        assert!(committed.state.has_account(account));
        if variants {
            assert_eq!(committed.state.balance(account), 1);
            assert_eq!(committed.state.balance(recipient), 1);
        }
        assert!(!committed.validator_safety_ready);
        node.process_prepared_task_sync(vec![deliver(abort)])
            .unwrap();
        assert_eq!(
            target.load().unwrap().unwrap().generation,
            committed.generation
        );
        let events = node.drain_bft_consensus_events().unwrap();
        assert_eq!(events.iter().filter(|event| matches!(event,
            BftConsensusEvent::CertifiedPreparedTask { task_id, .. } if *task_id == holder.task_id()
        )).count(), 1);
        drop(node);
        for (store, base) in [(source, source_base), (target, target_base)] {
            store.remove_files().unwrap();
            let _ = std::fs::remove_file(base.with_extension("lock"));
        }
    }
}
