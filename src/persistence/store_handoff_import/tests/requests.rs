//! A queued request survives handoff, but the handoff cannot renew its expiry.
use super::*;
use std::time::Duration;

#[tokio::test]
async fn queued_request_import_preserves_signed_body_and_does_not_extend_expiry() {
    let validators = ValidatorSet::new(1, validator_set().credentials().take(1).cloned()).unwrap();
    let next = ValidatorSet::new(2, validators.credentials().cloned()).unwrap();
    let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
    let account = crate::test_helpers::account(193);
    let (source, source_base) = temp_store();
    source
        .initialize(&SecondState::genesis([account], 1), &validators)
        .unwrap();
    let task = crate::test_helpers::sign(
        LegalTaskPayload::new(
            TaskId::parse("handoff-queued-expiry").unwrap(),
            1,
            Some(2),
            vec![Operation::Issue { account, count: 1 }],
        ),
        &key(9),
    )
    .unwrap()
    .verify(&authorizers)
    .unwrap();
    source.queue_currency_allocation(&task).unwrap();
    let trusted = source.load().unwrap().unwrap();
    let transition = source
        .prepare_validator_set_transition(
            ValidatorSetTransition::new(
                1,
                &validators,
                &trusted.validator_registry,
                next.clone(),
                vec![],
                vec![],
                1,
            )
            .unwrap(),
        )
        .unwrap();
    let bytes = transition.handoff.as_ref().unwrap().encode().unwrap();
    let statement = transition.finality_statement();
    let certified = CertifiedValidatorSetTransition::new(
        transition,
        vec![ValidatorVote::sign_unchecked(
            &statement,
            ValidatorId::new(1),
            &key(4),
        )],
        &validators,
    )
    .unwrap();
    let proof = ValidatorSetTransitionProof::from_certified(&certified);
    let (target, target_base) = temp_store();
    let wrong_authorizers = AuthorizerSet::new(1, [key(99).verifying_key().to_bytes()]).unwrap();
    assert!(
        target
            .install_validator_handoff_baseline(&proof, &bytes, &trusted, &wrong_authorizers)
            .is_err()
    );
    assert!(target.load().unwrap().is_none());
    target
        .install_validator_handoff_baseline(&proof, &bytes, &trusted, &authorizers)
        .unwrap();
    let cold = StateStore::new(&target_base).load().unwrap().unwrap();
    assert_eq!(
        cold.state.protocol.task_bindings[&task.task_id()]
            .allocation_task
            .as_ref(),
        Some(task.signed_task())
    );
    assert!(cold.prepared_tasks.is_empty());
    assert!(cold.bft_local_states.is_empty());
    assert!(!cold.validator_safety_ready);
    assert_eq!(cold.state.next_currency_address(), 1);
    assert_eq!(cold.state.balance(account), 0);
    assert_eq!(
        cold.state
            .protocol
            .task_handoff
            .as_ref()
            .unwrap()
            .encode()
            .unwrap(),
        bytes
    );
    StateRecoveryPayload::decode_bytes(
        &StateRecoveryPayload::from_persisted(&cold)
            .unwrap()
            .encode_bytes()
            .unwrap(),
    )
    .unwrap();

    source
        .activate_validator_set_transition(&certified)
        .unwrap();
    let runtime = NodeRuntime::bind_loaded(
        "127.0.0.1:0".parse().unwrap(),
        &source,
        StateStore::new(&source_base).load().unwrap().unwrap(),
        NodeRuntimeCapabilities::default().with_validator(
            ValidatorRuntimeKeys::new(ValidatorId::new(1), key(3), key(4)),
            ValidatorRuntimeConfig::new(
                authorizers,
                BftTimeoutConfig::new(
                    Duration::from_secs(1),
                    Duration::from_secs(1),
                    Duration::from_secs(1),
                ),
                || 3,
            ),
        ),
    )
    .unwrap();
    runtime.resume_currency_allocations().unwrap();
    let resumed = StateStore::new(&source_base).load().unwrap().unwrap();
    assert_eq!(
        resumed.state.protocol.task_bindings[&task.task_id()]
            .allocation_task
            .as_ref(),
        Some(task.signed_task())
    );
    assert!(resumed.prepared_tasks.is_empty());
    assert!(
        resumed.state.protocol.task_bindings[&task.task_id()]
            .allocation
            .is_none()
    );
    assert!(resumed.bft_local_states.is_empty());
    assert_eq!(resumed.state.next_currency_address(), 1);
    assert_eq!(resumed.state.balance(account), 0);
    assert_eq!(
        resumed
            .state
            .protocol
            .task_handoff
            .as_ref()
            .unwrap()
            .requests[&task.task_id()],
        *task.signed_task()
    );
    drop(runtime);
    for (store, base) in [(source, source_base), (target, target_base)] {
        store.remove_files().unwrap();
        let _ = std::fs::remove_file(base.with_extension("lock"));
        let _ = std::fs::remove_file(crate::transport_identity_path(&base));
        let _ = std::fs::remove_file(base.with_extension("transport.lock"));
    }
}
