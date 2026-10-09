use super::*;
use crate::*;
use ed25519_dalek::SigningKey;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::Instant;

#[tokio::test]
async fn abort_proposal_never_fetches_business_source_or_grants_unproven_voting_rights() {
    let (store, _) = crate::prepared::tests::temp_store();
    let validators = crate::prepared::tests::validator_set();
    let key = crate::prepared::tests::key;
    let authorizers = AuthorizerSet::new(
        CURRENT_PROTOCOL_VERSION,
        [key(9).verifying_key().to_bytes()],
    )
    .unwrap();
    let task_id = TaskId::parse("abort-source-sync").unwrap();
    let task = crate::test_helpers::sign(
        LegalTaskPayload::new(
            task_id.clone(),
            CURRENT_PROTOCOL_VERSION,
            None,
            vec![Operation::RegisterAccount {
                account: crate::test_helpers::account(82),
            }],
        ),
        &key(9),
    )
    .unwrap()
    .verify(&authorizers)
    .unwrap();
    let mut state = SecondState::genesis([], 1);
    store.initialize(&state, &validators).unwrap();
    PreparedTaskBook::new(store.clone())
        .unwrap()
        .prepare(&mut state, &task, 1, &validators)
        .unwrap();
    let runtime = NodeRuntime::bind_loaded(
        "127.0.0.1:0".parse().unwrap(),
        &store,
        store.load().unwrap().unwrap(),
        NodeRuntimeCapabilities::default().with_validator(
            ValidatorRuntimeKeys::new(ValidatorId::new(2), key(6), key(7)),
            ValidatorRuntimeConfig::new(
                authorizers,
                BftTimeoutConfig::new(
                    Duration::from_secs(2),
                    Duration::from_secs(2),
                    Duration::from_secs(2),
                ),
                || 1,
            ),
        ),
    )
    .unwrap();
    runtime
        .start_prepared_task_consensus(task_id.clone())
        .unwrap();
    let commit = store
        .prepared_bft_proposal_subject(task_id.clone())
        .unwrap();
    runtime
        .validator_bft
        .as_ref()
        .unwrap()
        .acknowledge_prepared_task_announcement(
            ValidatorId::new(1),
            1,
            commit.scope(),
            commit.digest(),
        );
    let abort = store.prepared_abort_statement(&task_id).unwrap();
    let scope = ConsensusScope::PreparedTask(task_id.clone());
    let proposal = ValidatorSigner::new(ValidatorId::new(1), key(4), store.clone())
        .sign_bft_proposal(
            &BftProposalSubject::new(1, scope.clone(), abort.subject_digest()),
            0,
            &validators,
        )
        .unwrap();
    let snapshot = store.load().unwrap().unwrap();
    let inbound = runtime
        .process_prepared_task_sync(vec![InboundBftMessage {
            validator_id: ValidatorId::new(1),
            message: BftNetworkMessage::Proposal {
                proposal,
                unlock_certificate: None,
            },
        }])
        .unwrap();
    let bft = runtime.validator_bft.as_ref().unwrap();
    assert!(!bft.has_pending_prepared_task_sync());
    assert_eq!(
        store.load().unwrap().unwrap().generation,
        snapshot.generation
    );
    let output = bft.consensus().drive(inbound, Instant::now());
    assert!(!output.outbound.iter().any(|message| matches!(
        message,
        BftNetworkMessage::Vote { statement, .. }
            if statement.value() == BftValue::Digest(abort.subject_digest())
    )));
    assert!(
        !store
            .load()
            .unwrap()
            .unwrap()
            .state
            .task_cancelled(task_id.clone())
    );

    let certificate = FinalityCertificate::new(
        abort,
        (1..=3)
            .map(|id| {
                ValidatorVote::sign_unchecked(
                    &abort,
                    ValidatorId::new(id),
                    &key((id * 3 + 1) as u8),
                )
            })
            .collect(),
        &validators,
    )
    .unwrap();
    bft.begin_prepared_task_fetch(ValidatorId::new(1), 1, scope.clone(), commit.digest(), 0);
    assert!(bft.has_pending_prepared_task_sync());
    let output = bft.consensus().drive(
        vec![InboundBftMessage {
            validator_id: ValidatorId::new(1),
            message: BftNetworkMessage::FinalityCertificate {
                scope: scope.clone(),
                certificate,
            },
        }],
        Instant::now(),
    );
    assert_eq!(output.completed_prepared_tasks, vec![task_id.clone()]);
    assert!(
        store
            .load()
            .unwrap()
            .unwrap()
            .state
            .task_cancelled(task_id.clone())
    );
    for completed in output.completed_prepared_tasks {
        bft.finish_prepared_task_sync(&ConsensusScope::PreparedTask(completed));
    }
    assert!(!bft.has_pending_prepared_task_sync());
    // A delayed earlier Commit QC must not revive the removed business source
    // after the task has reached certified Abort finality.
    let qc_statement = BftStatement::new(
        1,
        scope.clone(),
        0,
        BftPhase::Prevote,
        BftValue::Digest(commit.digest()),
    );
    let late_qc = BftQuorumCertificate::new(
        qc_statement.clone(),
        (1..=3)
            .map(|id| {
                BftVote::sign_unchecked(
                    &qc_statement,
                    ValidatorId::new(id),
                    &key((id * 3 + 1) as u8),
                )
            })
            .collect(),
        &validators,
    )
    .unwrap();
    let terminal_generation = store.load().unwrap().unwrap().generation;
    runtime
        .process_prepared_task_sync(vec![InboundBftMessage {
            validator_id: ValidatorId::new(1),
            message: BftNetworkMessage::QuorumCertificate(late_qc),
        }])
        .unwrap();
    assert!(!bft.has_pending_prepared_task_sync());
    assert_eq!(
        store.load().unwrap().unwrap().generation,
        terminal_generation
    );
    drop(runtime);
    // A restored terminal result also recognizes late relayed proposals without
    // reviving a source fetch after the frozen business plan has been removed.
    let restored = NodeRuntime::bind_loaded(
        "127.0.0.1:0".parse().unwrap(),
        &store,
        store.load().unwrap().unwrap(),
        NodeRuntimeCapabilities::default(),
    )
    .unwrap();
    assert!(
        restored
            .local_prepared_subject_matches(&scope, 1, abort.subject_digest())
            .unwrap()
    );
    assert!(
        !restored
            .local_prepared_subject_matches(&scope, 2, abort.subject_digest())
            .unwrap()
    );
    assert!(
        !restored
            .local_prepared_subject_matches(&scope, 1, [42; 32])
            .unwrap()
    );
    assert!(
        !restored
            .local_prepared_subject_matches(
                &ConsensusScope::PreparedTask(TaskId::parse("other-task").unwrap()),
                1,
                abort.subject_digest()
            )
            .unwrap()
    );
    drop(restored);
    store.remove_files().unwrap();
}

#[tokio::test]
async fn expired_allocation_source_requires_admission_or_quorum_evidence() {
    let key = |seed| SigningKey::from_bytes(&[seed; 32]);
    let id = ValidatorId::new(1);
    let validators = ValidatorSet::new(
        1,
        [ValidatorCredential::new(
            id,
            key(3).verifying_key().to_bytes(),
            key(4).verifying_key().to_bytes(),
            key(5).verifying_key().to_bytes(),
        )
        .unwrap()],
    )
    .unwrap();
    let authorizers = AuthorizerSet::new(
        CURRENT_PROTOCOL_VERSION,
        [key(9).verifying_key().to_bytes()],
    )
    .unwrap();
    let account = crate::test_helpers::account(81);
    let task = crate::test_helpers::sign(
        LegalTaskPayload::new(
            TaskId::parse("expired-allocation").unwrap(),
            CURRENT_PROTOCOL_VERSION,
            Some(1),
            vec![Operation::Issue { account, count: 1 }],
        ),
        &key(9),
    )
    .unwrap();
    let verified = task.verify(&authorizers).unwrap();
    let base = std::env::temp_dir().join(format!(
        "second-expired-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = StateStore::new(&base);
    store
        .initialize(&SecondState::genesis([account], 1), &validators)
        .unwrap();
    let runtime = NodeRuntime::bind_loaded(
        "127.0.0.1:0".parse().unwrap(),
        &store,
        store.load().unwrap().unwrap(),
        NodeRuntimeCapabilities::default().with_validator(
            ValidatorRuntimeKeys::new(id, key(3), key(4)),
            ValidatorRuntimeConfig::new(
                authorizers,
                BftTimeoutConfig::new(
                    Duration::from_secs(2),
                    Duration::from_secs(2),
                    Duration::from_secs(2),
                ),
                || 2,
            ),
        ),
    )
    .unwrap();
    let allocation = CurrencyAllocation::new(&verified, 1, 1).unwrap();
    let bft = runtime.validator_bft.as_ref().unwrap();
    bft.begin_prepared_task_fetch(id, 1, allocation.scope(), allocation.digest(), 0);
    runtime.process_prepared_task_sync(Vec::new()).unwrap();
    assert!(bft.has_pending_prepared_task_sync());
    let source = crate::legal_task_codec::encode_legal_task(&task).unwrap();
    assert!(matches!(
        runtime.install_fetched_prepared_task(1, &allocation.scope(), allocation.digest(), &source),
        Err(BftConsensusRuntimeError::Preparation(
            PreparationError::Execution(ExecutionError::TaskExpired)
        ))
    ));
    assert_eq!(
        store.load().unwrap().unwrap().state.next_currency_address(),
        1
    );
    let statement = allocation.finality_statement();
    let certificate = FinalityCertificate::new(
        statement,
        vec![ValidatorVote::sign_unchecked(&statement, id, &key(4))],
        &validators,
    )
    .unwrap();
    let consensus = runtime.validator_bft.as_ref().unwrap().consensus();
    consensus.drive(
        vec![InboundBftMessage {
            validator_id: id,
            message: BftNetworkMessage::FinalityCertificate {
                scope: allocation.scope(),
                certificate,
            },
        }],
        Instant::now(),
    );
    runtime
        .install_fetched_prepared_task(1, &allocation.scope(), allocation.digest(), &source)
        .unwrap();
    consensus.drive(Vec::new(), Instant::now());
    assert_eq!(
        store.load().unwrap().unwrap().state.next_currency_address(),
        2
    );
    runtime.process_prepared_task_sync(Vec::new()).unwrap();
    assert!(!bft.has_pending_prepared_task_sync());
    let generation = store.load().unwrap().unwrap().generation;
    runtime
        .process_prepared_task_sync(vec![InboundBftMessage {
            validator_id: id,
            message: BftNetworkMessage::PreparedTaskAvailable {
                validator_set_version: 1,
                scope: allocation.scope(),
                expected_plan_digest: [17; 32],
                round: 7,
            },
        }])
        .unwrap();
    assert!(!bft.has_pending_prepared_task_sync());
    assert_eq!(store.load().unwrap().unwrap().generation, generation);
    drop(runtime);
    store.remove_files().unwrap();
}
