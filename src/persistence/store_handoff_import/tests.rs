use crate::prepared::tests::{key, temp_store, validator_set};
use crate::*;

mod reentry;
mod requests;
mod terminal_delivery;

#[tokio::test]
async fn certified_baseline_import_admits_new_member_atomically_without_signing_rights() {
    let validators = validator_set();
    let account = crate::test_helpers::account(181);
    let pending_account = crate::test_helpers::account(182);
    let (source, source_base) = temp_store();
    let accounts = std::iter::once(account).chain((0u64..2100).map(|index| {
        let mut bytes = [184; 32];
        bytes[..8].copy_from_slice(&index.to_be_bytes());
        AccountAddress::from_bytes(bytes)
    }));
    let mut state = SecondState::genesis(accounts, 1).with_reserve(2).unwrap();
    source.initialize(&state, &validators).unwrap();
    let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
    let task = crate::test_helpers::sign(
        LegalTaskPayload::new(
            TaskId::parse("imported-handoff-obligation").unwrap(),
            1,
            Some(2),
            vec![Operation::RegisterAccount {
                account: pending_account,
            }],
        ),
        &key(9),
    )
    .unwrap()
    .verify(&authorizers)
    .unwrap();
    PreparedTaskBook::new(source.clone())
        .unwrap()
        .prepare(&mut state, &task, 1, &validators)
        .unwrap();
    let cancelled = crate::test_helpers::sign(
        LegalTaskPayload::new(
            TaskId::parse("imported-handoff-abort").unwrap(),
            1,
            Some(2),
            vec![Operation::RegisterAccount {
                account: crate::test_helpers::account(186),
            }],
        ),
        &key(9),
    )
    .unwrap()
    .verify(&authorizers)
    .unwrap();
    PreparedTaskBook::new(source.clone())
        .unwrap()
        .prepare(&mut state, &cancelled, 1, &validators)
        .unwrap();
    let abort_statement = source
        .prepared_abort_statement(&cancelled.task_id())
        .unwrap();
    let queued = crate::test_helpers::sign(
        LegalTaskPayload::new(
            TaskId::parse("imported-queued-request").unwrap(),
            1,
            None,
            vec![Operation::Issue { account, count: 1 }],
        ),
        &key(9),
    )
    .unwrap()
    .verify(&authorizers)
    .unwrap();
    source.queue_currency_allocation(&queued).unwrap();
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
    let next = ValidatorSet::new(
        2,
        validators
            .credentials()
            .filter(|c| c.id() != ValidatorId::new(4))
            .cloned()
            .chain(std::iter::once(joining)),
    )
    .unwrap();
    let transition = source
        .prepare_validator_set_transition(
            ValidatorSetTransition::new(
                1,
                &validators,
                &trusted.validator_registry,
                next.clone(),
                vec![admission],
                vec![],
                3,
            )
            .unwrap(),
        )
        .unwrap();
    let bytes = transition.handoff.as_ref().unwrap().encode().unwrap();
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
    let certified = CertifiedValidatorSetTransition::new(transition, votes, &validators).unwrap();
    let proof = ValidatorSetTransitionProof::from_certified(&certified);
    let (target, target_base) = temp_store();

    let mut corrupt_proof = proof.encode_bytes().unwrap();
    *corrupt_proof.last_mut().unwrap() ^= 1;
    let corrupt_proof = ValidatorSetTransitionProof::decode_bytes(&corrupt_proof).unwrap();
    assert!(
        target
            .install_validator_handoff_baseline(&corrupt_proof, &bytes, &trusted, &authorizers)
            .is_err()
    );
    assert!(target.load().unwrap().is_none());
    let mut corrupt_body = bytes.clone();
    *corrupt_body.last_mut().unwrap() ^= 1;
    assert!(
        target
            .install_validator_handoff_baseline(&proof, &corrupt_body, &trusted, &authorizers)
            .is_err()
    );
    assert!(target.load().unwrap().is_none());
    let untrusted_authorizers =
        AuthorizerSet::new(1, [key(99).verifying_key().to_bytes()]).unwrap();
    assert!(
        target
            .install_validator_handoff_baseline(&proof, &bytes, &trusted, &untrusted_authorizers)
            .is_err()
    );
    assert!(target.load().unwrap().is_none());
    let mut wrong_anchor = trusted.clone();
    wrong_anchor.validator_set = next.clone();
    assert!(
        target
            .install_validator_handoff_baseline(&proof, &bytes, &wrong_anchor, &authorizers)
            .is_err()
    );
    assert!(target.load().unwrap().is_none());

    let fetched = crate::network::fetch_from_current_member(
        &source,
        &certified,
        &trusted,
        &authorizers,
        &bytes,
    )
    .await;
    let generation = target
        .install_validator_handoff_baseline(&proof, &fetched, &trusted, &authorizers)
        .unwrap();
    let cold = StateStore::new(&target_base).load().unwrap().unwrap();
    assert_eq!(cold.validator_set, next);
    assert_eq!(
        cold.state.protocol.task_bindings[&queued.task_id()]
            .allocation_task
            .as_ref(),
        Some(queued.signed_task())
    );
    assert!(
        cold.state.protocol.task_bindings[&queued.task_id()]
            .allocation
            .is_none()
    );
    assert!(cold.state.business.accounts.contains(&account));
    assert!(
        !cold
            .state
            .business
            .accounts
            .contains(&crate::test_helpers::account(185))
    );
    assert!(!cold.state.business.accounts.contains(&pending_account));
    assert_eq!(cold.state.reserve_count(), 2);
    assert_eq!(
        cold.state.bound_request_digest(task.task_id()),
        Some(task.request_digest())
    );
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
    assert!(cold.retained_validator_sets.contains_key(&1));
    assert!(cold.prepared_tasks.is_empty());
    assert!(cold.validator_vote_locks.is_empty());
    assert!(cold.bft_local_states.is_empty());
    assert!(!cold.validator_safety_ready);
    assert_eq!(cold.minimum_signing_validator_set_version, 2);
    assert!(cold.recovery_checkpoint_proof.is_none());
    let checkpoint = StateRecoveryCheckpoint::from_persisted(1, &cold).unwrap();
    assert_eq!(
        ValidatorSigner::new(ValidatorId::new(5), key(16), target.clone())
            .sign_state_recovery_checkpoint(&checkpoint, &next),
        Err(ValidatorSigningError::LocalSafetyStateUnavailable)
    );
    assert_eq!(
        target.install_validator_handoff_baseline(&proof, &bytes, &trusted, &authorizers),
        Err(PersistenceError::AlreadyInitialized)
    );
    assert_eq!(target.load().unwrap().unwrap().generation, generation);
    // The same installed handoff is valid in the existing shared-recovery codec.
    StateRecoveryPayload::decode_bytes(
        &StateRecoveryPayload::from_persisted(&cold)
            .unwrap()
            .encode_bytes()
            .unwrap(),
    )
    .unwrap();
    // Advance unrelated business after importing the authenticated baseline.
    // The exact historical body still has admission evidence despite expiry.
    let mut imported_state = cold.state.clone();
    let mut imported_book = PreparedTaskBook::new(target.clone()).unwrap();
    let later_account = crate::test_helpers::account(183);
    let later = crate::test_helpers::sign(
        LegalTaskPayload::new(
            TaskId::parse("import-baseline-later-business").unwrap(),
            1,
            None,
            vec![Operation::RegisterAccount {
                account: later_account,
            }],
        ),
        &key(9),
    )
    .unwrap()
    .verify(&authorizers)
    .unwrap();
    imported_book
        .prepare(&mut imported_state, &later, 1, &next)
        .unwrap();
    let later_statement = imported_book
        .prepared_finality_statement(later.task_id())
        .unwrap();
    let later_certificate = FinalityCertificate::new(
        later_statement,
        (1..=3)
            .map(|id| {
                ValidatorVote::sign_unchecked(
                    &later_statement,
                    ValidatorId::new(id),
                    &key((id * 3 + 1) as u8),
                )
            })
            .collect(),
        &next,
    )
    .unwrap();
    imported_book
        .commit(&mut imported_state, later.task_id(), &later_certificate)
        .unwrap();
    let original_plan = &trusted.prepared_tasks[&task.task_id()];
    let original_digest = original_plan.plan_digest().unwrap();
    let before_late = target.load().unwrap().unwrap().generation;
    assert!(matches!(
        imported_book.prepare_expected_plan(
            &mut imported_state,
            &task,
            3,
            &validators,
            [0; 32],
            &[]
        ),
        Err(PreparationError::Execution(ExecutionError::TaskExpired))
    ));
    assert_eq!(target.load().unwrap().unwrap().generation, before_late);
    imported_book
        .prepare_expected_plan(
            &mut imported_state,
            &cancelled,
            3,
            &validators,
            trusted.prepared_tasks[&cancelled.task_id()]
                .plan_digest()
                .unwrap(),
            &[],
        )
        .unwrap();
    let original_statement = PreparedTaskBook::new(source.clone())
        .unwrap()
        .prepared_finality_statement(task.task_id())
        .unwrap();
    let original_certificate = FinalityCertificate::new(
        original_statement,
        (1..=3)
            .map(|id| {
                ValidatorVote::sign_unchecked(
                    &original_statement,
                    ValidatorId::new(id),
                    &key((id * 3 + 1) as u8),
                )
            })
            .collect(),
        &validators,
    )
    .unwrap();
    let runtime = NodeRuntime::bind_loaded(
        "127.0.0.1:0".parse().unwrap(),
        &target,
        target.load().unwrap().unwrap(),
        NodeRuntimeCapabilities::default().with_validator(
            ValidatorRuntimeKeys::new(ValidatorId::new(5), key(15), key(16)),
            ValidatorRuntimeConfig::new(
                authorizers.clone(),
                BftTimeoutConfig::new(
                    std::time::Duration::from_secs(1),
                    std::time::Duration::from_secs(1),
                    std::time::Duration::from_secs(1),
                ),
                || 3,
            ),
        ),
    )
    .unwrap();
    runtime.start_durable_prepared_consensus().unwrap();
    let before_proof = target.load().unwrap().unwrap().generation;
    let invalid_certificate = FinalityCertificate::from_untrusted_parts(
        original_statement,
        vec![ValidatorVote::from_untrusted_parts(
            ValidatorId::new(1),
            [0; 64],
        )],
    );
    assert!(
        runtime
            .process_prepared_task_sync(vec![crate::runtime_bft::InboundBftMessage {
                validator_id: ValidatorId::new(1),
                message: BftNetworkMessage::FinalityCertificate {
                    scope: ConsensusScope::PreparedTask(task.task_id()),
                    certificate: invalid_certificate,
                },
            }])
            .unwrap()
            .is_empty()
    );
    assert_eq!(target.load().unwrap().unwrap().generation, before_proof);
    let abort_certificate = FinalityCertificate::new(
        abort_statement,
        (1..=3)
            .map(|id| {
                ValidatorVote::sign_unchecked(
                    &abort_statement,
                    ValidatorId::new(id),
                    &key((id * 3 + 1) as u8),
                )
            })
            .collect(),
        &validators,
    )
    .unwrap();
    terminal_delivery::deliver(
        &source,
        &runtime,
        &authorizers,
        [
            BftNetworkMessage::FinalityCertificate {
                scope: ConsensusScope::PreparedTask(cancelled.task_id()),
                certificate: abort_certificate,
            },
            BftNetworkMessage::FinalityCertificate {
                scope: ConsensusScope::PreparedTask(task.task_id()),
                certificate: original_certificate,
            },
        ],
    )
    .await;
    let events = runtime.drain_bft_consensus_events().unwrap();
    assert_eq!(events.iter().filter(|event| matches!(event,
        BftConsensusEvent::CertifiedPreparedTask { task_id, .. } if *task_id == cancelled.task_id()
    )).count(), 1, "historical cancellation must notify completion without another business certificate");
    assert_eq!(events.iter().filter(|event| matches!(event,
        BftConsensusEvent::CertifiedPreparedTask { task_id, .. } if *task_id == task.task_id()
    )).count(), 1, "passive completion must use the normal receipt event and deduplication");
    let bft = runtime.validator_bft.as_ref().unwrap();
    assert!(
        !bft.has_pending_prepared_task_sync(),
        "installed bodies require no private source fetch"
    );
    assert!(
        !bft.consensus()
            .drive(vec![], tokio::time::Instant::now())
            .outbound
            .iter()
            .any(|message| matches!(
                message,
                BftNetworkMessage::Vote { .. } | BftNetworkMessage::FinalityVote { .. }
            ))
    );
    drop(runtime);
    let completed = StateStore::new(&target_base).load().unwrap().unwrap();
    assert_eq!(completed.state.task_succeeded(task.task_id()), Some(true));
    assert_eq!(
        completed.task_receipts[&task.task_id()]
            .plan()
            .plan_digest()
            .unwrap(),
        original_digest
    );
    assert!(completed.state.task_cancelled(cancelled.task_id()));
    assert!(
        !completed
            .state
            .business
            .accounts
            .contains(&crate::test_helpers::account(186))
    );
    assert!(completed.prepared_tasks.is_empty());
    assert!(completed.state.business.accounts.contains(&pending_account));
    assert!(completed.state.business.accounts.contains(&later_account));
    assert!(!completed.validator_safety_ready);
    assert_eq!(completed.minimum_signing_validator_set_version, 2);
    assert!(completed.validator_vote_locks.is_empty());
    assert!(completed.bft_local_states.is_empty());
    let (empty_source, empty_source_base) = temp_store();
    empty_source
        .initialize(&SecondState::genesis([], 1), &validators)
        .unwrap();
    let empty_trusted = empty_source.load().unwrap().unwrap();
    let empty_transition = empty_source
        .prepare_validator_set_transition(
            ValidatorSetTransition::new(
                1,
                &validators,
                &empty_trusted.validator_registry,
                next,
                proof
                    .source()
                    .admissions()
                    .iter()
                    .map(|request| request.verify().unwrap())
                    .collect(),
                vec![],
                1,
            )
            .unwrap(),
        )
        .unwrap();
    let empty_body = empty_transition.handoff.as_ref().unwrap().encode().unwrap();
    assert!(empty_transition.handoff_digest.is_none());
    let statement = empty_transition.finality_statement();
    let certificate = CertifiedValidatorSetTransition::new(
        empty_transition,
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
    let empty_proof = ValidatorSetTransitionProof::from_certified(&certificate);
    let mut invalid_empty_proof = empty_proof.encode_bytes().unwrap();
    *invalid_empty_proof.last_mut().unwrap() ^= 1;
    let invalid_empty_proof =
        ValidatorSetTransitionProof::decode_bytes(&invalid_empty_proof).unwrap();
    let (empty_target, empty_target_base) = temp_store();
    assert!(
        empty_target
            .install_validator_handoff_baseline(
                &invalid_empty_proof,
                &empty_body,
                &empty_trusted,
                &authorizers
            )
            .is_err()
    );
    assert!(empty_target.load().unwrap().is_none());
    empty_target
        .install_validator_handoff_baseline(&empty_proof, &empty_body, &empty_trusted, &authorizers)
        .unwrap();
    let empty_cold = StateStore::new(&empty_target_base).load().unwrap().unwrap();
    assert!(empty_cold.state.protocol.task_handoff.is_none());
    assert!(empty_cold.validator_transition_proofs.contains_key(&1));
    assert!(!empty_cold.validator_safety_ready);
    StateRecoveryPayload::decode_bytes(
        &StateRecoveryPayload::from_persisted(&empty_cold)
            .unwrap()
            .encode_bytes()
            .unwrap(),
    )
    .unwrap();
    for (store, base) in [
        (source, source_base),
        (target, target_base),
        (empty_source, empty_source_base),
        (empty_target, empty_target_base),
    ] {
        store.remove_files().unwrap();
        let _ = std::fs::remove_file(base.with_extension("lock"));
    }
}
