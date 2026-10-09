//! A final certificate is a cut, even when its body omits a local unsigned duty.
use super::*;

#[tokio::test]
async fn live_certificate_fetch_applies_exact_cut_without_voting_for_an_omitted_root() {
    let validators = validator_set();
    let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
    let initial = SecondState::genesis([], 1);
    let (provider, provider_base) = temp_store();
    let (receiver, receiver_base) = temp_store();
    for store in [&provider, &receiver] {
        store.initialize(&initial, &validators).unwrap();
    }
    let request = |name, byte| {
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
    let covered = request("live-cut-covered", 231);
    let omitted = request("live-cut-omitted", 232);
    for (store, task) in [(&provider, &covered), (&receiver, &omitted)] {
        PreparedTaskBook::new(store.clone())
            .unwrap()
            .prepare(&mut initial.clone(), task, 1, &validators)
            .unwrap();
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
    provider
        .admit_governance(
            &crate::runtime_consensus_target::ValidatorConsensusTarget::ValidatorSetTransition(
                transition.clone(),
            ),
        )
        .unwrap();
    let (_, source) = source_chunk(
        &provider.load().unwrap().unwrap(),
        &transition.scope(),
        transition.digest(),
        0,
    )
    .unwrap();
    let certificate = FinalityCertificate::new(
        transition.finality_statement(),
        (1..=3)
            .map(|id| {
                ValidatorVote::sign_unchecked(
                    &transition.finality_statement(),
                    ValidatorId::new(id),
                    &key((id * 3 + 1) as u8),
                )
            })
            .collect(),
        &validators,
    )
    .unwrap();
    let node = NodeRuntime::bind_loaded(
        "127.0.0.1:0".parse().unwrap(),
        &receiver,
        receiver.load().unwrap().unwrap(),
        NodeRuntimeCapabilities::default().with_validator(
            ValidatorRuntimeKeys::new(ValidatorId::new(4), key(12), key(13)),
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
    // No unsigned smaller body can close the local task or authorize its vote.
    let before = receiver.load().unwrap().unwrap().generation;
    assert_eq!(
        receiver.prepare_validator_set_transition(transition.clone()),
        Err(PersistenceError::StalePreparedTasks)
    );
    let message = |certificate| InboundBftMessage {
        validator_id: ValidatorId::new(1),
        message: BftNetworkMessage::FinalityCertificate {
            scope: transition.scope(),
            certificate,
        },
    };
    assert!(
        node.process_governance_bft_sources(vec![message(
            FinalityCertificate::from_untrusted_parts(transition.finality_statement(), vec![])
        )])
        .is_empty()
    );
    assert_eq!(receiver.load().unwrap().unwrap().generation, before);
    let pending = node.process_governance_bft_sources(vec![message(certificate.clone())]);
    assert_eq!(pending.len(), 1);
    node.validator_bft
        .as_ref()
        .unwrap()
        .consensus()
        .drive(pending, tokio::time::Instant::now());
    node.install_transition_handoff_source(1, &transition.scope(), transition.digest(), &source)
        .unwrap();
    let cold = StateStore::new(&receiver_base).load().unwrap().unwrap();
    assert_eq!(cold.validator_set.version(), 2);
    assert_eq!(
        cold.state.protocol.task_handoff.as_ref(),
        transition.handoff.as_ref()
    );
    assert!(cold.validator_vote_locks.is_empty());
    assert!(cold.bft_local_states.is_empty());
    assert!(!cold.prepared_tasks[&covered.task_id()].commit_authorized);
    assert!(!cold.prepared_tasks.contains_key(&omitted.task_id()));
    assert_eq!(
        cold.state.protocol.task_bindings[&omitted.task_id()]
            .allocation_task
            .as_ref(),
        Some(omitted.signed_task())
    );
    assert!(cold.state.business.accounts.is_empty());
    assert_eq!(cold.state.next_currency_address(), 1);
    drop(node);
    // A previously authenticated root remains usable after one of its covered
    // tasks commits. Do not rebuild that plan against today's business state.
    let statement = PreparedTaskBook::new(provider.clone())
        .unwrap()
        .prepared_finality_statement(covered.task_id())
        .unwrap();
    let business_certificate = FinalityCertificate::new(
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
    .unwrap();
    PreparedTaskBook::commit_certified_from_store(
        &provider,
        covered.task_id(),
        &business_certificate,
    )
    .unwrap();
    let warm = provider.load().unwrap().unwrap();
    let warm_generation = warm.generation;
    let warm_node = NodeRuntime::bind_loaded(
        "127.0.0.1:0".parse().unwrap(),
        &provider,
        warm,
        NodeRuntimeCapabilities::default().with_validator(
            ValidatorRuntimeKeys::new(ValidatorId::new(4), key(12), key(13)),
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
    assert!(
        warm_node
            .process_governance_bft_sources(vec![message(certificate)])
            .is_empty()
    );
    let installed = StateStore::new(&provider_base).load().unwrap().unwrap();
    assert_eq!(installed.generation, warm_generation + 1);
    assert_eq!(installed.validator_set.version(), 2);
    assert!(
        installed
            .state
            .has_account(crate::test_helpers::account(231))
    );
    assert_eq!(
        installed.state.task_succeeded(covered.task_id()),
        Some(true)
    );
    assert_eq!(
        installed.state.protocol.task_handoff.as_ref(),
        transition.handoff.as_ref()
    );
    assert!(installed.validator_vote_locks.is_empty());
    drop(warm_node);
    for (store, base) in [(provider, provider_base), (receiver, receiver_base)] {
        store.remove_files().unwrap();
        for suffix in ["lock", "transport", "transport.lock", "peers"] {
            let _ = std::fs::remove_file(base.with_extension(suffix));
        }
    }
}
