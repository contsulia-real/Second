use crate::prepared::tests::{key, temp_store, validator_set};
use crate::*;

fn request(name: &str, byte: u8, authorizers: &AuthorizerSet) -> VerifiedLegalTask {
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
    .verify(authorizers)
    .unwrap()
}

fn certify(
    transition: ValidatorSetTransition,
    validators: &ValidatorSet,
) -> CertifiedValidatorSetTransition {
    let statement = transition.finality_statement();
    CertifiedValidatorSetTransition::new(
        transition,
        (1..=3)
            .map(|id| {
                ValidatorVote::sign_unchecked(
                    &statement,
                    ValidatorId::new(id),
                    &key((id * 3 + 1) as u8),
                )
            })
            .collect(),
        validators,
    )
    .unwrap()
}

fn next(store: &StateStore, validators: &ValidatorSet) -> ValidatorSetTransition {
    let snapshot = store.load_shared().unwrap().unwrap();
    store
        .prepare_validator_set_transition(
            ValidatorSetTransition::new(
                1,
                validators,
                &snapshot.validator_registry,
                ValidatorSet::new(2, validators.credentials().cloned()).unwrap(),
                vec![],
                vec![],
                1,
            )
            .unwrap(),
        )
        .unwrap()
}

#[tokio::test]
async fn certified_omission_retires_rights_atomically_and_cold_retries_original_request() {
    let validators = validator_set();
    let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
    let (provider, provider_base) = temp_store();
    let (store, base) = temp_store();
    let initial = SecondState::genesis([], 1);
    for target in [&provider, &store] {
        target.initialize(&initial, &validators).unwrap();
    }
    let omitted = request("certified-cut-omitted", 91, &authorizers);
    PreparedTaskBook::new(store.clone())
        .unwrap()
        .prepare(&mut initial.clone(), &omitted, 1, &validators)
        .unwrap();
    let covered = request("certified-cut-covered", 92, &authorizers);
    PreparedTaskBook::new(provider.clone())
        .unwrap()
        .prepare(&mut initial.clone(), &covered, 1, &validators)
        .unwrap();
    let transition = next(&provider, &validators);
    let certified = certify(transition.clone(), &validators);
    let scope = ConsensusScope::PreparedTask(omitted.task_id());
    let bind = |snapshot: PersistedNodeState| {
        NodeRuntime::bind_loaded(
            "127.0.0.1:0".parse().unwrap(),
            &store,
            snapshot,
            NodeRuntimeCapabilities::default().with_validator(
                ValidatorRuntimeKeys::new(ValidatorId::new(4), key(12), key(13)),
                ValidatorRuntimeConfig::new(
                    authorizers.clone(),
                    BftTimeoutConfig::new(
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
    let hot = bind(store.load().unwrap().unwrap());
    hot.start_prepared_task_consensus(omitted.task_id())
        .unwrap();
    let signer = ValidatorSigner::new(ValidatorId::new(4), key(13), store.clone());
    signer
        .sign_bft_prevote(scope.clone(), 0, BftValue::Nil, &validators, None)
        .unwrap();
    let before = store.load_shared().unwrap().unwrap();
    // Neither an unsigned body nor a valid certificate with an unknown body
    // can retire the local task. Failed staging must leave no request queue.
    assert_eq!(
        store.prepare_validator_set_transition(transition.clone()),
        Err(PersistenceError::StalePreparedTasks)
    );
    assert_eq!(
        store.activate_validator_set_transition(&certified),
        Err(PersistenceError::StalePreparedTasks)
    );
    assert_eq!(
        store.load_shared().unwrap().unwrap().generation,
        before.generation
    );
    assert!(
        store
            .load_shared()
            .unwrap()
            .unwrap()
            .state
            .protocol
            .task_bindings[&omitted.task_id()]
            .allocation_task
            .is_none()
    );
    let mut witnessed_state = before.state.clone();
    PreparedTaskBook::new(store.clone())
        .unwrap()
        .collect_transition_handoff(&mut witnessed_state, &transition, &authorizers, 1)
        .unwrap();
    let witnessed = store.load_shared().unwrap().unwrap();
    assert!(!witnessed.prepared_tasks[&covered.task_id()].commit_authorized);
    store.activate_validator_set_transition(&certified).unwrap();
    let cold = StateStore::new(&base).load().unwrap().unwrap();
    assert_eq!(cold.generation, witnessed.generation + 1);
    assert_eq!(cold.validator_set.version(), 2);
    assert!(!cold.prepared_tasks.contains_key(&omitted.task_id()));
    assert!(
        !cold
            .bft_local_states
            .contains_key(&(ValidatorId::new(4), scope.clone()))
    );
    assert!(cold.validator_vote_locks.is_empty());
    assert_eq!(
        cold.state.protocol.task_bindings[&omitted.task_id()]
            .allocation_task
            .as_ref(),
        Some(omitted.signed_task())
    );
    assert_eq!(
        cold.state.bound_request_digest(omitted.task_id()),
        Some(omitted.request_digest())
    );
    assert!(!cold.prepared_tasks[&covered.task_id()].commit_authorized);
    let generation = cold.generation;
    assert!(
        signer
            .sign_bft_prevote(scope.clone(), 0, BftValue::Nil, &validators, None)
            .is_err()
    );
    assert_eq!(store.load_shared().unwrap().unwrap().generation, generation);
    hot.validator_bft
        .as_ref()
        .unwrap()
        .consensus()
        .retire_certified_omissions(&cold)
        .unwrap();
    let node = bind(cold);
    node.resume_currency_allocations().unwrap();
    let retried = store.load_shared().unwrap().unwrap();
    let plan = &retried.prepared_tasks[&omitted.task_id()];
    assert_eq!(plan.validator_set_version, 2);
    assert_eq!(plan.source_task, *omitted.signed_task());
    assert!(plan.commit_authorized);
    hot.start_prepared_task_consensus(omitted.task_id())
        .unwrap();
    assert!(
        retried.state.protocol.task_bindings[&omitted.task_id()]
            .allocation_task
            .is_none()
    );
    assert!(
        signer
            .sign_bft_prevote(scope, 0, BftValue::Nil, &validators, None)
            .is_err()
    );
    let statement = PreparedTaskBook::new(store.clone())
        .unwrap()
        .prepared_finality_statement(omitted.task_id())
        .unwrap();
    let certificate = FinalityCertificate::new(
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
        &retried.validator_set,
    )
    .unwrap();
    PreparedTaskBook::commit_certified_from_store(&store, omitted.task_id(), &certificate).unwrap();
    let completed = StateStore::new(&base).load().unwrap().unwrap();
    assert_eq!(
        completed.state.task_succeeded(omitted.task_id()),
        Some(true)
    );
    assert!(
        completed
            .state
            .has_account(crate::test_helpers::account(91))
    );
    assert_eq!(completed.state.next_currency_address(), 1);
    drop(node);
    drop(hot);
    for (target, path) in [(store, base), (provider, provider_base)] {
        target.remove_files().unwrap();
        for suffix in ["lock", "transport", "transport.lock", "peers"] {
            let _ = std::fs::remove_file(path.with_extension(suffix));
        }
    }
}

#[test]
fn certified_cut_never_retires_commit_or_abort_quorum_evidence() {
    let validators = validator_set();
    let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
    for abort in [false, true] {
        let (store, base) = temp_store();
        let initial = SecondState::genesis([], 1);
        store.initialize(&initial, &validators).unwrap();
        let certified = certify(next(&store, &validators), &validators);
        let task = request("protected-certified-cut", 93, &authorizers);
        PreparedTaskBook::new(store.clone())
            .unwrap()
            .prepare(&mut initial.clone(), &task, 1, &validators)
            .unwrap();
        let digest = if abort {
            store
                .prepared_abort_statement(&task.task_id())
                .unwrap()
                .subject_digest()
        } else {
            store
                .prepared_bft_proposal_subject(task.task_id())
                .unwrap()
                .digest()
        };
        let scope = ConsensusScope::PreparedTask(task.task_id());
        let statement = BftStatement::new(
            1,
            scope.clone(),
            0,
            BftPhase::Precommit,
            BftValue::Digest(digest),
        );
        let qc = BftQuorumCertificate::new(
            statement.clone(),
            (1..=3)
                .map(|id| {
                    BftVote::sign_unchecked(
                        &statement,
                        ValidatorId::new(id),
                        &key((id * 3 + 1) as u8),
                    )
                })
                .collect(),
            &validators,
        )
        .unwrap();
        store
            .accept_bft_precommit_qc(ValidatorId::new(4), &qc, &validators)
            .unwrap();
        let before = StateStore::new(&base).load().unwrap().unwrap();
        assert_eq!(
            store.activate_validator_set_transition(&certified),
            Err(PersistenceError::StalePreparedTasks)
        );
        let after = StateStore::new(&base).load().unwrap().unwrap();
        assert_eq!(after.generation, before.generation);
        assert_eq!(after.validator_set, validators);
        assert_eq!(after.prepared_tasks, before.prepared_tasks);
        assert_eq!(after.bft_local_states, before.bft_local_states);
        assert!(after.prepared_tasks[&task.task_id()].commit_authorized);
        assert!(
            after.state.protocol.task_bindings[&task.task_id()]
                .allocation_task
                .is_none()
        );
        store.remove_files().unwrap();
        let _ = std::fs::remove_file(base.with_extension("lock"));
    }
}
