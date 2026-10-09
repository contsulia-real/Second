use super::*;
use crate::prepared::tests::{key, temp_store, validator_set};
use crate::*;
use std::time::Duration;

mod automatic;
mod live_cut;

#[tokio::test]
async fn chunked_handoff_merges_missing_bodies_without_importing_claims_and_rejects_invalid_sources()
 {
    let validators = validator_set();
    let alice = crate::test_helpers::account(131);
    let bob = crate::test_helpers::account(132);
    let source = PaymentAddress::from_bytes([131; 32]);
    let destination = PaymentAddress::from_bytes([132; 32]);
    let mut initial = SecondState::genesis([alice, bob], 3);
    for (address, account) in [(source, alice), (destination, bob)] {
        initial.business.payment_addresses.insert(
            address,
            crate::payment::PaymentAddressRecord {
                account,
                status: PaymentAddressStatus::Active,
            },
        );
    }
    for number in 1..=2 {
        let address = CurrencyAddress::new(number);
        initial.business.currencies.insert(
            address,
            crate::currency::Currency {
                address,
                role: CurrencyRole::Circulation,
                owner: Some(alice),
            },
        );
    }
    let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
    let mut operations = vec![Operation::Transfer {
        source,
        destination,
        amount: 1,
    }];
    for number in 0u16..512 {
        operations.push(Operation::RegisterAccount {
            account: crate::test_helpers::account_u16(number),
        });
    }
    let mut task = crate::test_helpers::sign(
        LegalTaskPayload::new(
            TaskId::parse("handoff-missing-variant").unwrap(),
            1,
            None,
            operations,
        ),
        &key(9),
    )
    .unwrap();
    for number in 0u16..512 {
        task.add_account_signature(&crate::test_helpers::key_u16(number))
            .unwrap();
    }
    let task = task.verify(&authorizers).unwrap();
    let extra = crate::test_helpers::sign(
        LegalTaskPayload::new(
            TaskId::parse("handoff-new-body").unwrap(),
            1,
            None,
            vec![Operation::RegisterAccount {
                account: crate::test_helpers::account(133),
            }],
        ),
        &key(9),
    )
    .unwrap()
    .verify(&authorizers)
    .unwrap();
    let (provider_store, provider_base) = temp_store();
    let (receiver_store, receiver_base) = temp_store();
    for store in [&provider_store, &receiver_store] {
        store.initialize(&initial, &validators).unwrap();
        let mut state = initial.clone();
        PreparedTaskBook::new(store.clone())
            .unwrap()
            .prepare(&mut state, &task, 1, &validators)
            .unwrap();
    }
    let mut state = provider_store.load().unwrap().unwrap().state;
    let mut book = PreparedTaskBook::new(provider_store.clone()).unwrap();
    let mut alternative = provider_store
        .load_prepared_tasks()
        .unwrap()
        .remove(&task.task_id())
        .unwrap();
    let crate::prepared_plan::PreparedOperation::Transfer { currencies, .. } =
        &mut alternative.operations[0]
    else {
        panic!("transfer fixture")
    };
    *currencies = vec![CurrencyAddress::new(2)];
    let alternative_digest = alternative.plan_digest().unwrap();
    let selections = vec![vec![CurrencyAddress::new(2)]];
    assert!(
        book.admit_frozen_variant(
            &mut state,
            &task,
            &validators,
            alternative_digest,
            &selections
        )
        .unwrap()
        .is_none()
    );
    book.prepare(&mut state, &extra, 1, &validators).unwrap();
    let snapshot = provider_store.load().unwrap().unwrap();
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
    let transition = provider_store
        .prepare_validator_set_transition(bare)
        .unwrap();
    provider_store
        .admit_governance(&ValidatorConsensusTarget::ValidatorSetTransition(
            transition.clone(),
        ))
        .unwrap();
    let snapshot = provider_store.load().unwrap().unwrap();
    let mut encoded = Vec::new();
    let mut chunks = 0;
    loop {
        let (total, bytes) = source_chunk(
            &snapshot,
            &transition.scope(),
            transition.digest(),
            encoded.len() as u64,
        )
        .unwrap();
        assert!(bytes.len() <= crate::network::MAX_PREPARED_TASK_SOURCE_CHUNK_SIZE);
        encoded.extend_from_slice(&bytes);
        chunks += 1;
        if encoded.len() as u64 == total {
            break;
        }
    }
    assert!(chunks > 1);
    let bind = |store: &StateStore, authorizers, id| {
        NodeRuntime::bind_loaded(
            "127.0.0.1:0".parse().unwrap(),
            store,
            store.load().unwrap().unwrap(),
            NodeRuntimeCapabilities::default().with_validator(
                ValidatorRuntimeKeys::new(
                    ValidatorId::new(id),
                    key((id * 3) as u8),
                    key((id * 3 + 1) as u8),
                ),
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
        .unwrap()
    };
    let unauthorized = bind(
        &receiver_store,
        AuthorizerSet::new(1, [key(99).verifying_key().to_bytes()]).unwrap(),
        2,
    );
    let generation = receiver_store.load().unwrap().unwrap().generation;
    assert!(
        unauthorized
            .install_transition_handoff_source(
                1,
                &transition.scope(),
                transition.digest(),
                &encoded
            )
            .is_err()
    );
    assert_eq!(
        receiver_store.load().unwrap().unwrap().generation,
        generation
    );
    drop(unauthorized);
    let receiver = bind(&receiver_store, authorizers.clone(), 2);
    let mut incomplete = snapshot.clone();
    incomplete
        .prepared_tasks
        .retain(|id, _| id == &extra.task_id());
    let handoff = crate::persistence::TaskHandoff::capture(&incomplete).unwrap();
    let omission = transition
        .clone()
        .with_handoff_arc(std::sync::Arc::new(handoff))
        .unwrap();
    let source = ValidatorSetTransitionSource::from_transition(&omission)
        .encode_bytes()
        .unwrap();
    let mut omitted = DOMAIN.to_vec();
    omitted.push(0);
    omitted.extend_from_slice(&(source.len() as u32).to_be_bytes());
    omitted.extend_from_slice(&source);
    omitted.extend_from_slice(&omission.handoff.as_ref().unwrap().encode().unwrap());
    receiver
        .install_transition_handoff_source(1, &omission.scope(), omission.digest(), &omitted)
        .unwrap();
    let augmented = receiver_store.load().unwrap().unwrap();
    assert!(
        !augmented
            .pending_governance
            .contains_key(&omission.digest())
    );
    assert!(augmented.prepared_tasks[&task.task_id()].commit_authorized);
    assert!(augmented.pending_governance.values().all(|pending| {
        pending
            .transition()
            .unwrap()
            .handoff
            .as_ref()
            .unwrap()
            .task_context(&task.task_id())
            .is_some()
    }));
    let generation = augmented.generation;
    let mut corrupted = encoded.clone();
    *corrupted.last_mut().unwrap() ^= 1;
    assert!(
        receiver
            .install_transition_handoff_source(
                1,
                &transition.scope(),
                transition.digest(),
                &corrupted
            )
            .is_err()
    );
    assert_eq!(
        receiver_store.load().unwrap().unwrap().generation,
        generation
    );
    let initial_receiver = receiver_store.load().unwrap().unwrap();
    let (network_store, network_base) = temp_store();
    network_store
        .initialize(&initial_receiver.state, &validators)
        .unwrap();
    network_store
        .replace_prepared_tasks(&Default::default(), &initial_receiver.prepared_tasks)
        .unwrap();
    let network_receiver = std::sync::Arc::new(bind(&network_store, authorizers.clone(), 2));
    let network_provider = std::sync::Arc::new(bind(&provider_store, authorizers, 1));
    let (records, workers) =
        connect_handoff_nodes(&[network_provider.clone(), network_receiver.clone()]);
    network_receiver
        .dial_validator_bft(&records[0])
        .await
        .unwrap();
    network_provider
        .dial_validator_bft(&records[1])
        .await
        .unwrap();
    assert!(
        network_provider
            .validator_bft
            .as_ref()
            .unwrap()
            .broadcast(&BftNetworkMessage::ValidatorSetTransitionSource {
                collecting: false,
                validator_set_version: 1,
                scope: transition.scope(),
                bytes: ValidatorSetTransitionSource::from_transition(&transition)
                    .encode_bytes()
                    .unwrap(),
            })
            .is_empty()
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            for node in [&network_receiver, &network_provider] {
                let bft = node.validator_bft.as_ref().unwrap();
                let messages = node.process_governance_bft_sources(bft.drain_inbound());
                node.process_prepared_task_sync(messages).unwrap();
            }
            if network_store
                .load()
                .unwrap()
                .unwrap()
                .pending_governance
                .contains_key(&transition.digest())
            {
                break;
            }
            assert!(workers.iter().all(|worker| !worker.is_finished()));
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("authenticated QUIC must pull and admit the missing handoff bodies");
    let network = network_store.load().unwrap().unwrap();
    assert!(!network.prepared_tasks[&extra.task_id()].commit_authorized);
    assert_eq!(network.state.balance(bob), 0);
    assert_eq!(
        PreparedTaskBook::new(network_store.clone())
            .unwrap()
            .claimed_currency_count(),
        1
    );
    for worker in &workers {
        worker.abort();
    }
    for worker in workers {
        let _ = worker.await;
    }
    drop(network_receiver);
    drop(network_provider);
    network_store.remove_files().unwrap();
    let _ = std::fs::remove_file(network_base.with_extension("lock"));
    receiver
        .install_transition_handoff_source(1, &transition.scope(), transition.digest(), &encoded)
        .unwrap();
    let cold = StateStore::new(&receiver_base).load().unwrap().unwrap();
    assert!(
        cold.state
            .same_persisted_state(&receiver_store.load().unwrap().unwrap().state)
    );
    assert_eq!(cold.state.balance(bob), 0);
    let plan = &cold.prepared_tasks[&task.task_id()];
    assert!(
        plan.candidate(alternative_digest)
            .unwrap()
            .is_some_and(|candidate| !candidate.commit_authorized)
    );
    assert!(!cold.prepared_tasks[&extra.task_id()].commit_authorized);
    assert_eq!(
        PreparedTaskBook::new(receiver_store.clone())
            .unwrap()
            .claimed_currency_count(),
        1
    );
    let signer = ValidatorSigner::new(ValidatorId::new(2), key(7), receiver_store.clone());
    assert!(
        signer
            .sign_prepared_task(
                task.task_id(),
                &FinalityStatement::new(1, 1, alternative_digest),
                &validators
            )
            .is_err()
    );
    let imported = receiver_store
        .prepare_validator_set_transition(transition.clone())
        .unwrap();
    assert_eq!(imported.digest(), transition.digest());
    let before = cold.generation;
    receiver
        .install_transition_handoff_source(1, &transition.scope(), transition.digest(), &encoded)
        .unwrap();
    assert_eq!(receiver_store.load().unwrap().unwrap().generation, before);
    drop(receiver);
    let statement = transition.finality_statement();
    let votes = (1..=3)
        .map(|id| {
            ValidatorVote::sign_unchecked(
                &statement,
                ValidatorId::new(id),
                &key((id * 3 + 1) as u8),
            )
        })
        .collect();
    provider_store
        .activate_validator_set_transition(
            &CertifiedValidatorSetTransition::new(transition.clone(), votes, &validators).unwrap(),
        )
        .unwrap();
    let activated = StateStore::new(&provider_base).load().unwrap().unwrap();
    assert_eq!(activated.validator_set.version(), 2);
    assert!(
        !activated
            .pending_governance
            .contains_key(&transition.digest())
    );
    let mut late_source = Vec::new();
    loop {
        let (total, bytes) = source_chunk(
            &activated,
            &transition.scope(),
            transition.digest(),
            late_source.len() as u64,
        )
        .expect("cold activated provider must retain the certified handoff source");
        late_source.extend_from_slice(&bytes);
        if late_source.len() as u64 == total {
            break;
        }
    }
    assert_eq!(late_source, encoded);
    assert!(source_chunk(&activated, &transition.scope(), [0; 32], 0).is_none());
    let wrong_scope = ConsensusScope::CurrencyAllocation {
        validator_set_version: 2,
        start: 3,
    };
    assert!(source_chunk(&activated, &wrong_scope, transition.digest(), 0).is_none());
    assert!(
        source_chunk(
            &activated,
            &transition.scope(),
            transition.digest(),
            encoded.len() as u64,
        )
        .is_none()
    );
    assert_eq!(
        provider_store.load().unwrap().unwrap().generation,
        activated.generation
    );
    for (store, path) in [
        (provider_store, provider_base),
        (receiver_store, receiver_base),
    ] {
        store.remove_files().unwrap();
        let _ = std::fs::remove_file(path.with_extension("lock"));
    }
}

#[tokio::test]
async fn chunked_collection_requires_atomic_promotion_and_resumes_membership_candidates() {
    let validators = validator_set();
    let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
    let initial = SecondState::genesis([crate::test_helpers::account(181)], 1);
    let (provider, provider_base) = temp_store();
    let (receiver, receiver_base) = temp_store();
    for (store, name, byte) in [
        (&provider, "collection-source-only", 182),
        (&receiver, "collection-receiver-only", 183),
    ] {
        store.initialize(&initial, &validators).unwrap();
        let task = crate::test_helpers::sign(
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
        .unwrap();
        PreparedTaskBook::new(store.clone())
            .unwrap()
            .prepare(&mut initial.clone(), &task, 1, &validators)
            .unwrap();
    }
    let queued = crate::test_helpers::sign(
        LegalTaskPayload::new(
            TaskId::parse("collection-source-unallocated").unwrap(),
            1,
            Some(2),
            vec![Operation::Issue {
                account: crate::test_helpers::account(181),
                count: 1,
            }],
        ),
        &key(9),
    )
    .unwrap();
    provider
        .queue_currency_allocation(&queued.clone().verify(&authorizers).unwrap())
        .unwrap();
    let snapshot = provider.load().unwrap().unwrap();
    let bare = ValidatorSetTransition::new(
        1,
        &validators,
        &snapshot.validator_registry,
        ValidatorSet::new(2, validators.credentials().cloned()).unwrap(),
        vec![],
        vec![],
        1,
    )
    .unwrap();
    let source = provider.prepare_validator_set_transition(bare).unwrap();
    let contribution = PreparedTaskBook::new(provider.clone())
        .unwrap()
        .collect_transition_handoff(&mut snapshot.state.clone(), &source, &authorizers, 1)
        .unwrap();
    let cold_provider = StateStore::new(&provider_base).load().unwrap().unwrap();
    let (total, bytes) = source_chunk(
        &cold_provider,
        &contribution.scope(),
        contribution.digest(),
        0,
    )
    .unwrap();
    assert_eq!(total as usize, bytes.len());
    let node = NodeRuntime::bind_loaded(
        "127.0.0.1:0".parse().unwrap(),
        &receiver,
        receiver.load().unwrap().unwrap(),
        NodeRuntimeCapabilities::default().with_validator(
            ValidatorRuntimeKeys::new(ValidatorId::new(2), key(6), key(7)),
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
    let generation = receiver.load().unwrap().unwrap().generation;
    let mut invalid = bytes.clone();
    invalid[DOMAIN.len()] = 2;
    assert!(
        node.install_transition_handoff_source(
            1,
            &contribution.scope(),
            contribution.digest(),
            &invalid
        )
        .is_err()
    );
    assert_eq!(receiver.load().unwrap().unwrap().generation, generation);
    node.install_transition_handoff_source(1, &contribution.scope(), contribution.digest(), &bytes)
        .unwrap();
    let cold = StateStore::new(&receiver_base).load().unwrap().unwrap();
    assert_eq!(cold.generation, generation + 1);
    assert_eq!(cold.pending_governance.len(), 1);
    let pending = cold.pending_governance.values().next().unwrap();
    assert!(pending.target().is_none());
    let union = pending.transition().unwrap();
    assert_ne!(union.digest(), contribution.digest());
    // A validated subset differs from our union, but repeated metadata must not
    // reopen its fetch or grant the subset any voting authority.
    let repeated_source = InboundBftMessage {
        validator_id: ValidatorId::new(1),
        message: BftNetworkMessage::ValidatorSetTransitionSource {
            collecting: true,
            validator_set_version: 1,
            scope: contribution.scope(),
            bytes: ValidatorSetTransitionSource::from_transition(&contribution)
                .encode_bytes()
                .unwrap(),
        },
    };
    assert!(
        node.process_governance_bft_sources(vec![repeated_source])
            .is_empty()
    );
    assert!(
        !node
            .validator_bft
            .as_ref()
            .unwrap()
            .has_pending_prepared_task_sync()
    );
    assert!(node.drain_bft_consensus_events().unwrap().is_empty());
    assert_eq!(union.handoff.as_ref().unwrap().plans.len(), 2);
    assert_eq!(
        union
            .handoff
            .as_ref()
            .unwrap()
            .requests
            .get(&queued.payload().task_id()),
        Some(&queued)
    );
    assert_eq!(
        cold.state.protocol.task_bindings[&queued.payload().task_id()]
            .allocation_task
            .as_ref(),
        Some(&queued)
    );
    assert!(
        !cold
            .prepared_tasks
            .contains_key(&queued.payload().task_id())
    );
    assert!(
        !cold.prepared_tasks[&TaskId::parse("collection-source-only").unwrap()].commit_authorized
    );
    assert!(
        receiver
            .validator_transition_bft_proposal_subject(union)
            .is_err()
    );
    node.resume_governance().unwrap();
    node.start_validator_set_transition_consensus(union.clone())
        .unwrap();
    assert!(node.drain_bft_consensus_events().unwrap().is_empty());
    assert_eq!(
        receiver.load().unwrap().unwrap().generation,
        cold.generation
    );
    crate::prepared::tests::advance_nil_round(
        &receiver,
        &validators,
        ValidatorId::new(2),
        union.scope(),
        0,
    );
    node.advance_transition_collections().unwrap();
    assert_eq!(
        receiver.load().unwrap().unwrap().generation,
        cold.generation + 2
    );
    assert!(cold.validator_vote_locks.is_empty());
    assert!(cold.bft_local_states.is_empty());
    let receiver_node = std::sync::Arc::new(node);
    let provider_node = std::sync::Arc::new(
        NodeRuntime::bind_loaded(
            "127.0.0.1:0".parse().unwrap(),
            &provider,
            provider.load().unwrap().unwrap(),
            NodeRuntimeCapabilities::default().with_validator(
                ValidatorRuntimeKeys::new(ValidatorId::new(1), key(3), key(4)),
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
    let (records, workers) = connect_handoff_nodes(&[provider_node.clone(), receiver_node.clone()]);
    receiver_node.dial_validator_bft(&records[0]).await.unwrap();
    provider_node.dial_validator_bft(&records[1]).await.unwrap();
    assert_eq!(
        provider_node
            .collect_validator_set_transition(source)
            .unwrap()
            .digest(),
        contribution.digest(),
    );
    crate::prepared::tests::advance_nil_round(
        &provider,
        &validators,
        ValidatorId::new(1),
        contribution.scope(),
        0,
    );
    provider_node.advance_transition_collections().unwrap();
    // The first announcement was lost before connections existed. Cold runtime
    // recovery must restart the same durable collection, with no manual body copy.
    receiver_node.resume_governance().unwrap();
    provider_node.resume_governance().unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            for node in [&provider_node, &receiver_node] {
                let bft = node.validator_bft.as_ref().unwrap();
                let pass = node.process_governance_bft_sources(bft.drain_inbound());
                let pass = node.process_prepared_task_sync(pass).unwrap();
                node.advance_transition_collections().unwrap();
                let output = bft.consensus().drive(pass, tokio::time::Instant::now());
                assert!(!output.validator_set_changed);
                for message in output.outbound {
                    assert!(bft.broadcast(&message).is_empty());
                }
                bft.retry_prepared_task_sync(false);
            }
            let provider_cold = StateStore::new(&provider_base).load().unwrap().unwrap();
            if provider_cold
                .pending_governance
                .contains_key(&union.digest())
            {
                break;
            }
            assert!(workers.iter().all(|worker| !worker.is_finished()));
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("cold authenticated peers must automatically exchange their distinct obligations");
    provider_node.resume_governance().unwrap();
    let provider_cold = StateStore::new(&provider_base).load().unwrap().unwrap();
    assert_eq!(provider_cold.pending_governance.len(), 2);
    assert!(
        provider_cold
            .pending_governance
            .values()
            .all(|value| value.target().is_some())
    );
    assert!(
        !provider_cold.prepared_tasks[&TaskId::parse("collection-receiver-only").unwrap()]
            .commit_authorized
    );
    assert!(provider_cold.validator_vote_locks.is_empty());
    assert!(
        provider_cold
            .bft_local_states
            .keys()
            .all(|(_, scope)| !matches!(scope, ConsensusScope::PreparedTask(_)))
    );
    let receiver_cold = StateStore::new(&receiver_base).load().unwrap().unwrap();
    assert_eq!(
        receiver_cold.state.protocol.task_bindings[&queued.payload().task_id()]
            .allocation_task
            .as_ref(),
        Some(&queued)
    );
    assert!(
        receiver_cold.state.protocol.task_bindings[&queued.payload().task_id()]
            .allocation
            .is_none()
    );
    assert!(
        !receiver_cold
            .prepared_tasks
            .contains_key(&queued.payload().task_id())
    );
    assert!(receiver_cold.validator_vote_locks.is_empty());
    assert!(
        receiver_cold
            .bft_local_states
            .keys()
            .all(|(_, scope)| !matches!(scope, ConsensusScope::PreparedTask(_)))
    );
    for worker in &workers {
        worker.abort();
    }
    for worker in workers {
        let _ = worker.await;
    }
    drop(provider_node);
    drop(receiver_node);
    for (store, base) in [(provider, provider_base), (receiver, receiver_base)] {
        store.remove_files().unwrap();
        let _ = std::fs::remove_file(base.with_extension("lock"));
    }
}

fn connect_handoff_nodes(
    nodes: &[std::sync::Arc<NodeRuntime>],
) -> (Vec<crate::PeerRecord>, Vec<tokio::task::JoinHandle<()>>) {
    let mut records = Vec::new();
    let mut workers = Vec::new();
    for node in nodes {
        records.push(node.local_peer_record().unwrap().clone());
        let node = std::sync::Arc::clone(node);
        workers.push(tokio::spawn(async move {
            node.run_listener().await.unwrap();
        }));
    }
    (records, workers)
}

#[tokio::test]
async fn empty_genesis_collection_body_uses_the_canonical_absent_commitment() {
    let validators = validator_set();
    let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
    let (provider, provider_base) = temp_store();
    let (receiver, receiver_base) = temp_store();
    let initial = SecondState::genesis([], 1);
    for store in [&provider, &receiver] {
        store.initialize(&initial, &validators).unwrap();
    }
    let snapshot = provider.load().unwrap().unwrap();
    let transition = provider
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
    assert!(transition.handoff_digest.is_none());
    PreparedTaskBook::new(provider.clone())
        .unwrap()
        .collect_transition_handoff(&mut initial.clone(), &transition, &authorizers, 1)
        .unwrap();
    let (_, bytes) = source_chunk(
        &provider.load().unwrap().unwrap(),
        &transition.scope(),
        transition.digest(),
        0,
    )
    .unwrap();
    let node = NodeRuntime::bind_loaded(
        "127.0.0.1:0".parse().unwrap(),
        &receiver,
        receiver.load().unwrap().unwrap(),
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
    .unwrap();
    node.install_transition_handoff_source(1, &transition.scope(), transition.digest(), &bytes)
        .unwrap();
    let cold = StateStore::new(&receiver_base).load().unwrap().unwrap();
    assert!(
        cold.pending_governance
            .get(&transition.digest())
            .is_some_and(|value| value.target().is_none())
    );
    assert!(cold.prepared_tasks.is_empty());
    assert!(cold.validator_vote_locks.is_empty());
    assert!(cold.bft_local_states.is_empty());
    drop(node);
    for (store, base) in [(provider, provider_base), (receiver, receiver_base)] {
        store.remove_files().unwrap();
        let _ = std::fs::remove_file(base.with_extension("lock"));
    }
}
