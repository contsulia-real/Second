use crate::prepared::tests::{key, temp_store, validator_set};
use crate::runtime_bft::InboundBftMessage;
use crate::*;
use std::time::Duration;

#[tokio::test]
async fn certified_expired_source_keeps_time_eligibility_but_revalidates_changed_business() {
    let validators = validator_set();
    let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
    let account = crate::test_helpers::account(194);
    let task = crate::test_helpers::sign(
        LegalTaskPayload::new(
            TaskId::parse("certified-expired-source").unwrap(),
            1,
            Some(2),
            vec![Operation::RegisterAccount { account }],
        ),
        &key(9),
    )
    .unwrap()
    .verify(&authorizers)
    .unwrap();
    let (provider, provider_base) = temp_store();
    let mut state = SecondState::genesis([], 1);
    provider.initialize(&state, &validators).unwrap();
    let mut book = PreparedTaskBook::new(provider.clone()).unwrap();
    book.prepare(&mut state, &task, 1, &validators).unwrap();
    let plan = provider
        .load_prepared_tasks()
        .unwrap()
        .remove(&task.task_id())
        .unwrap();
    let digest = plan.plan_digest().unwrap();
    let source = plan.encode_source().unwrap();
    let scope = ConsensusScope::PreparedTask(task.task_id());
    let certify = |statement| {
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
        .unwrap()
    };
    let certificate = certify(book.prepared_finality_statement(task.task_id()).unwrap());
    for (precommit, change_business) in [(false, false), (false, true), (true, false), (true, true)]
    {
        let (store, base) = temp_store();
        store
            .initialize(&SecondState::genesis([], 1), &validators)
            .unwrap();
        let runtime = NodeRuntime::bind_loaded(
            "127.0.0.1:0".parse().unwrap(),
            &store,
            store.load().unwrap().unwrap(),
            NodeRuntimeCapabilities::default().with_validator(
                ValidatorRuntimeKeys::new(ValidatorId::new(1), key(3), key(4)),
                ValidatorRuntimeConfig::new(
                    authorizers.clone(),
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
        let generation = store.load_shared().unwrap().unwrap().generation;
        assert!(matches!(
            runtime.install_fetched_prepared_task(1, &scope, digest, &source),
            Err(BftConsensusRuntimeError::Preparation(
                PreparationError::Execution(ExecutionError::TaskExpired)
            ))
        ));
        assert_eq!(store.load_shared().unwrap().unwrap().generation, generation);
        let bft = runtime.validator_bft.as_ref().unwrap();
        let deliver = |certificate| InboundBftMessage {
            validator_id: ValidatorId::new(2),
            message: BftNetworkMessage::FinalityCertificate {
                scope: scope.clone(),
                certificate,
            },
        };
        // A valid quorum for another digest is not admission of this source.
        let wrong = certify(FinalityStatement::new(1, 1, [98; 32]));
        let inbound = runtime
            .process_prepared_task_sync(vec![deliver(wrong)])
            .unwrap();
        bft.consensus().drive(inbound, tokio::time::Instant::now());
        assert!(matches!(
            runtime.install_fetched_prepared_task(1, &scope, digest, &source),
            Err(BftConsensusRuntimeError::Preparation(
                PreparationError::Execution(ExecutionError::TaskExpired)
            ))
        ));
        let precommit_message = |scope: ConsensusScope, phase, value, count| {
            let statement = BftStatement::new(1, scope, 0, phase, value);
            BftNetworkMessage::QuorumCertificate(BftQuorumCertificate::from_untrusted_parts(
                statement.clone(),
                (1..=count)
                    .map(|id| {
                        BftVote::sign_unchecked(
                            &statement,
                            ValidatorId::new(id),
                            &key((id * 3 + 1) as u8),
                        )
                    })
                    .collect(),
            ))
        };
        if precommit {
            for message in [
                precommit_message(
                    scope.clone(),
                    BftPhase::Prevote,
                    BftValue::Digest(digest),
                    3,
                ),
                precommit_message(
                    scope.clone(),
                    BftPhase::Precommit,
                    BftValue::Digest(digest),
                    2,
                ),
                precommit_message(
                    scope.clone(),
                    BftPhase::Precommit,
                    BftValue::Digest([99; 32]),
                    3,
                ),
                precommit_message(
                    ConsensusScope::PreparedTask(TaskId::parse("other-expired-task").unwrap()),
                    BftPhase::Precommit,
                    BftValue::Digest(digest),
                    3,
                ),
            ] {
                bft.consensus().drive(
                    vec![InboundBftMessage {
                        validator_id: ValidatorId::new(2),
                        message,
                    }],
                    tokio::time::Instant::now(),
                );
                assert!(matches!(
                    runtime.install_fetched_prepared_task(1, &scope, digest, &source),
                    Err(BftConsensusRuntimeError::Preparation(
                        PreparationError::Execution(ExecutionError::TaskExpired)
                    ))
                ));
                assert_eq!(store.load_shared().unwrap().unwrap().generation, generation);
            }
        }
        let evidence = if precommit {
            InboundBftMessage {
                validator_id: ValidatorId::new(2),
                message: precommit_message(
                    scope.clone(),
                    BftPhase::Precommit,
                    BftValue::Digest(digest),
                    3,
                ),
            }
        } else {
            deliver(certificate.clone())
        };
        let inbound = runtime.process_prepared_task_sync(vec![evidence]).unwrap();
        bft.consensus().drive(inbound, tokio::time::Instant::now());
        assert_eq!(store.load_shared().unwrap().unwrap().generation, generation);
        assert!(store.load_prepared_tasks().unwrap().is_empty());
        if change_business {
            let other = crate::test_helpers::sign(
                LegalTaskPayload::new(
                    TaskId::parse("business-before-certified-source").unwrap(),
                    1,
                    None,
                    vec![Operation::RegisterAccount { account }],
                ),
                &key(9),
            )
            .unwrap()
            .verify(&authorizers)
            .unwrap();
            let mut state = store.load().unwrap().unwrap().state;
            let mut book = PreparedTaskBook::new(store.clone()).unwrap();
            book.prepare(&mut state, &other, 3, &validators).unwrap();
            let proof = certify(book.prepared_finality_statement(other.task_id()).unwrap());
            book.commit(&mut state, other.task_id(), &proof).unwrap();
            let generation = store.load_shared().unwrap().unwrap().generation;
            assert!(matches!(
                runtime.install_fetched_prepared_task(1, &scope, digest, &source),
                Err(BftConsensusRuntimeError::Preparation(
                    PreparationError::Execution(ExecutionError::AccountAlreadyExists(value))
                )) if value == account
            ));
            assert_eq!(store.load_shared().unwrap().unwrap().generation, generation);
        } else if precommit {
            runtime
                .install_fetched_prepared_task(1, &scope, digest, &source)
                .unwrap();
            let output = bft
                .consensus()
                .drive(Vec::new(), tokio::time::Instant::now());
            assert!(output.outbound.iter().any(|message| matches!(message,
                BftNetworkMessage::FinalityVote { statement, .. } if statement.subject_digest() == digest)));
            let cold = StateStore::new(&base).load().unwrap().unwrap();
            assert_eq!(cold.state.task_succeeded(task.task_id()), Some(false));
            assert!(
                cold.prepared_tasks[&task.task_id()]
                    .finality_votes
                    .is_none()
            );
        } else {
            runtime
                .install_fetched_prepared_task(1, &scope, digest, &source)
                .unwrap();
            let events = runtime.drain_bft_consensus_events().unwrap();
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event,
                        BftConsensusEvent::CertifiedPreparedTask { task_id, certificate: actual }
                            if task_id == &task.task_id() && actual == &certificate
                    ))
                    .count(),
                1
            );
            let output = bft
                .consensus()
                .drive(Vec::new(), tokio::time::Instant::now());
            assert!(output.outbound.iter().all(|message| !matches!(
                message,
                BftNetworkMessage::Vote { .. } | BftNetworkMessage::FinalityVote { .. }
            )));
            let cold = StateStore::new(&base).load().unwrap().unwrap();
            assert_eq!(cold.state.task_succeeded(task.task_id()), Some(true));
            assert!(cold.state.business.accounts.contains(&account));
            assert!(cold.prepared_tasks.is_empty());
            assert!(cold.bft_local_states.is_empty());
            assert!(cold.validator_vote_locks.is_empty());
            assert_eq!(
                cold.task_receipts[&task.task_id()].certificate().unwrap(),
                certificate
            );
        }
        drop(runtime);
        store.remove_files().unwrap();
        std::fs::remove_file(&base).unwrap();
        std::fs::remove_file(crate::transport_identity_path(&base)).unwrap();
        std::fs::remove_file(base.with_extension("transport.lock")).unwrap();
    }
    provider.remove_files().unwrap();
    std::fs::remove_file(provider_base).unwrap();
}
