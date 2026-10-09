use super::*;

#[test]
fn restarted_nil_voter_advances_without_conflicting_proposal_or_deadline_renewal() {
    use crate::prepared::tests::{key, temp_store, validator_set};
    use crate::{AuthorizerSet, LegalTaskPayload, Operation};

    let validators = ValidatorSet::new(1, validator_set().credentials().take(1).cloned()).unwrap();
    let (store, base) = temp_store();
    let account = crate::test_helpers::account(219);
    store
        .initialize(&crate::SecondState::genesis([account], 1), &validators)
        .unwrap();
    let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
    let task = crate::test_helpers::sign(
        LegalTaskPayload::new(
            TaskId::parse("idle-session-round-recovery").unwrap(),
            1,
            None,
            vec![Operation::Issue { account, count: 1 }],
        ),
        &key(9),
    )
    .unwrap()
    .verify(&authorizers)
    .unwrap();
    let allocation = crate::CurrencyAllocation::new(&task, 1, 1).unwrap();
    let scope = allocation.scope();
    let signer = ValidatorSigner::new(ValidatorId::new(1), key(4), store.clone());
    let timeouts = BftTimeoutConfig::new(
        Duration::from_secs(1),
        Duration::from_secs(1),
        Duration::from_secs(1),
    );
    // Restart must resume the persisted phase, rather than proposing another
    // value or deleting the Nil vote. Its normal timeout forms the next QC.
    signer
        .sign_bft_prevote(scope.clone(), 0, BftValue::Nil, &validators, None)
        .unwrap();
    let mut coordinator = BftConsensusCoordinator::new();
    let target = ValidatorConsensusTarget::CurrencyAllocation(allocation.clone());
    coordinator
        .register(
            signer.clone(),
            store.clone(),
            validators.clone(),
            target.clone(),
            timeouts,
        )
        .unwrap();
    let now = Instant::now();
    let initial = coordinator.drive(vec![], now);
    assert!(
        initial.outbound.is_empty(),
        "restart proposed over its persisted Nil vote"
    );
    assert!(coordinator.drain_events().is_empty());
    let deadline = coordinator
        .next_deadline()
        .expect("restart failed to schedule its durable phase");
    let generation = store.load().unwrap().unwrap().generation;
    coordinator
        .register(
            signer.clone(),
            store.clone(),
            validators.clone(),
            target.clone(),
            timeouts,
        )
        .unwrap();
    assert_eq!(coordinator.next_deadline(), Some(deadline));
    // Duplicate registration while active must not renew the phase budget.
    coordinator
        .register(
            signer.clone(),
            store.clone(),
            validators.clone(),
            target.clone(),
            timeouts,
        )
        .unwrap();
    assert_eq!(coordinator.next_deadline(), Some(deadline));
    assert!(coordinator.drive(vec![], now).outbound.is_empty());
    assert_eq!(store.load().unwrap().unwrap().generation, generation);
    let output = coordinator.drive(vec![], deadline);
    assert!(
        output.currency_allocation_committed,
        "persisted Nil vote stalled instead of advancing through its normal QC"
    );
    let cold = crate::StateStore::new(&base).load().unwrap().unwrap();
    assert_eq!(cold.state.next_currency_address(), 2);
    let binding = &cold.state.protocol.task_bindings[&task.task_id()];
    assert_eq!(binding.allocation, Some((1, 1)));
    assert_eq!(binding.request_digest, task.request_digest());
    assert!(binding.allocation_certificate.is_some());
    let deadline = coordinator.next_deadline();
    let generation = cold.generation;
    assert!(matches!(
        coordinator.register(signer, store.clone(), validators, target, timeouts),
        Err(BftConsensusRuntimeError::Persistence(
            PersistenceError::StaleState
        ))
    ));
    assert_eq!(coordinator.next_deadline(), deadline);
    coordinator.drive(vec![], now);
    assert_eq!(store.load().unwrap().unwrap().generation, generation);
    store.remove_files().unwrap();
    let _ = std::fs::remove_file(base.with_extension("lock"));
}

#[tokio::test]
async fn pending_source_retry_deadline_wins_over_continuous_activity() {
    let consensus = ValidatorConsensusRuntime::new();
    let deadline = add_duration(Instant::now(), Duration::from_millis(40));
    let mut notifications = tokio::time::interval(Duration::from_millis(5));
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            notifications.tick().await;
            consensus.wake();
            if consensus
                .wait_for_activity_or_deadline(None, Some(deadline))
                .await
            {
                break;
            }
        }
    })
    .await
    .expect("activity must not renew the original retry deadline");
    // Even an already queued notification cannot defeat an expired retry.
    consensus.wake();
    assert!(
        consensus
            .wait_for_activity_or_deadline(None, Some(Instant::now()))
            .await
    );
    // Idle operation still waits only for actual activity, without a sync timer.
    assert!(!consensus.wait_for_activity_or_deadline(None, None).await);
}

#[test]
fn undrained_diagnostics_remain_bounded_and_keep_the_latest_failures() {
    let mut coordinator = BftConsensusCoordinator::new();
    for id in 0..1000 {
        coordinator.record_event(BftConsensusEvent::Rejected {
            validator_id: None,
            scope: ConsensusScope::PreparedTask(
                TaskId::parse(&format!("diagnostic-{id}")).unwrap(),
            ),
            error: BftConsensusRuntimeError::InvalidPreparedTaskSource,
        });
    }
    let events = coordinator.drain_events();
    assert_eq!(events.len(), 256);
    assert!(
        matches!(&events[0], BftConsensusEvent::Rejected { scope, .. } if scope == &ConsensusScope::PreparedTask(TaskId::parse("diagnostic-744").unwrap()))
    );
}

#[test]
fn restarted_allocation_proposer_recovers_qc_and_carries_unlock_proof_for_selected_candidate() {
    run_allocation_qc_recovery(false);
}

#[test]
fn late_prevote_qc_is_retained_without_rewinding_or_resigning_and_survives_restart() {
    run_allocation_qc_recovery(true);
}

fn run_allocation_qc_recovery(late: bool) {
    use crate::{AuthorizerSet, LegalTaskPayload, Operation, ValidatorCredential};
    use ed25519_dalek::SigningKey;
    let key = |seed| SigningKey::from_bytes(&[seed; 32]);
    let validators = ValidatorSet::new(
        1,
        (1..=4).map(|id| {
            ValidatorCredential::new(
                ValidatorId::new(id),
                key((id * 3) as u8).verifying_key().to_bytes(),
                key((id * 3 + 1) as u8).verifying_key().to_bytes(),
                key((id * 3 + 2) as u8).verifying_key().to_bytes(),
            )
            .unwrap()
        }),
    )
    .unwrap();
    let authorizers = AuthorizerSet::new(
        crate::CURRENT_PROTOCOL_VERSION,
        [key(9).verifying_key().to_bytes()],
    )
    .unwrap();
    let account = crate::test_helpers::account(91);
    let allocation = |name| {
        let task = crate::test_helpers::sign(
            LegalTaskPayload::new(
                TaskId::parse(name).unwrap(),
                crate::CURRENT_PROTOCOL_VERSION,
                None,
                vec![Operation::Issue { account, count: 1 }],
            ),
            &key(9),
        )
        .unwrap()
        .verify(&authorizers)
        .unwrap();
        crate::CurrencyAllocation::new(&task, validators.version(), 1).unwrap()
    };
    let first = allocation("qc-first");
    let selected = allocation("qc-selected");
    let base = std::env::temp_dir().join(format!(
        "second-qc-restart-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = crate::StateStore::new(&base);
    store
        .initialize(&crate::SecondState::genesis([account], 1), &validators)
        .unwrap();
    let signer = ValidatorSigner::new(ValidatorId::new(1), key(4), store.clone());
    let timeouts = BftTimeoutConfig::new(
        std::time::Duration::from_secs(2),
        std::time::Duration::from_secs(2),
        std::time::Duration::from_secs(2),
    );
    // Persist a lock and its proof, then move to this validator's next proposer round.
    let statement = crate::BftStatement::new(
        validators.version(),
        selected.scope(),
        0,
        BftPhase::Prevote,
        BftValue::Digest(selected.digest()),
    );
    let votes = (1..=3)
        .map(|id| {
            crate::BftVote::sign_unchecked(
                &statement,
                ValidatorId::new(id),
                &key((id * 3 + 1) as u8),
            )
        })
        .collect();
    let proof = BftQuorumCertificate::new(statement, votes, &validators).unwrap();
    if late {
        signer
            .sign_bft_prevote(selected.scope(), 0, BftValue::Nil, &validators, None)
            .unwrap();
    } else {
        signer
            .sign_bft_precommit(
                selected.scope(),
                0,
                BftValue::Digest(selected.digest()),
                &validators,
                Some(&proof),
            )
            .unwrap();
    }
    for round in 1..=if late { 3 } else { 4 } {
        store
            .advance_bft_round(ValidatorId::new(1), &selected.scope(), round)
            .unwrap();
    }
    if late {
        let mut receiver = BftConsensusCoordinator::new();
        for candidate in [&first, &selected] {
            receiver
                .register(
                    signer.clone(),
                    store.clone(),
                    validators.clone(),
                    ValidatorConsensusTarget::CurrencyAllocation(candidate.clone()),
                    timeouts,
                )
                .unwrap();
        }
        let message = InboundBftMessage {
            validator_id: ValidatorId::new(2),
            message: BftNetworkMessage::QuorumCertificate(proof.clone()),
        };
        let now = Instant::now();
        let output = receiver.drive(vec![message.clone()], now);
        assert!(
            output.outbound.is_empty(),
            "an old QC must not cause a new vote in the current round"
        );
        let local = store
            .bft_local_state(ValidatorId::new(1), &selected.scope())
            .unwrap()
            .unwrap();
        assert_eq!(local.round(), 3);
        assert_eq!(local.locked_round(), None);
        assert_eq!(local.valid_prevote_qc(), Some(&proof));
        let generation = store.load().unwrap().unwrap().generation;
        receiver.drive(vec![message], now);
        assert_eq!(store.load().unwrap().unwrap().generation, generation);
        let forged = BftQuorumCertificate::from_untrusted_parts(
            proof.statement().clone(),
            (1..=3)
                .map(|id| {
                    crate::BftVote::sign_unchecked(
                        proof.statement(),
                        ValidatorId::new(id),
                        &key(99),
                    )
                })
                .collect(),
        );
        receiver.drive(
            vec![InboundBftMessage {
                validator_id: ValidatorId::new(2),
                message: BftNetworkMessage::QuorumCertificate(forged),
            }],
            now,
        );
        assert!(receiver.drain_events().iter().any(|event| matches!(
            event,
            BftConsensusEvent::Rejected {
                error: BftConsensusRuntimeError::Driver(BftDriverError::Bft(
                    crate::BftError::InvalidSignature(_)
                )),
                ..
            }
        )));
        assert_eq!(store.load().unwrap().unwrap().generation, generation);
        store
            .advance_bft_round(ValidatorId::new(1), &selected.scope(), 4)
            .unwrap();
    }
    let restarted = crate::StateStore::new(&base);
    let mut coordinator = BftConsensusCoordinator::new();
    for candidate in [&first, &selected] {
        coordinator
            .register(
                signer.clone(),
                restarted.clone(),
                validators.clone(),
                ValidatorConsensusTarget::CurrencyAllocation(candidate.clone()),
                timeouts,
            )
            .unwrap();
    }
    let now = Instant::now() + std::time::Duration::from_secs(100);
    let output = coordinator.drive(Vec::new(), now);
    let (proposal, unlock_certificate) = output
        .outbound
        .iter()
        .find_map(|message| match message {
            BftNetworkMessage::Proposal {
                proposal,
                unlock_certificate,
            } => Some((proposal, unlock_certificate)),
            _ => None,
        })
        .expect("the restarted proposer must create a proposal with its recovered QC");
    assert_eq!(proposal.round(), 4);
    assert_eq!(proposal.subject_digest(), selected.digest());
    assert_eq!(unlock_certificate.as_ref(), Some(&proof));
    proposal.verify(&validators).unwrap();
    let generation = store.load().unwrap().unwrap().generation;
    // The network entry must reject an unverified QC before selecting another
    // candidate or remembering its proof, even though both bodies are known.
    let forged_statement = crate::BftStatement::new(
        validators.version(),
        first.scope(),
        4,
        BftPhase::Prevote,
        BftValue::Digest(first.digest()),
    );
    let forged = BftQuorumCertificate::from_untrusted_parts(
        forged_statement.clone(),
        (1..=3)
            .map(|id| {
                crate::BftVote::sign_unchecked(&forged_statement, ValidatorId::new(id), &key(99))
            })
            .collect(),
    );
    let rejected = coordinator.drive(
        vec![InboundBftMessage {
            validator_id: ValidatorId::new(2),
            message: BftNetworkMessage::QuorumCertificate(forged.clone()),
        }],
        now,
    );
    assert!(rejected.outbound.is_empty());
    assert!(coordinator.drain_events().iter().any(|event| matches!(
        event,
        BftConsensusEvent::Rejected {
            error: BftConsensusRuntimeError::Driver(BftDriverError::Bft(
                crate::BftError::InvalidSignature(_)
            )),
            ..
        }
    )));
    let session = &coordinator.sessions[&selected.scope()];
    assert_eq!(session.subject.digest(), selected.digest());
    assert_eq!(session.valid_prevote_qc.as_ref(), Some(&proof));
    assert_eq!(store.load().unwrap().unwrap().generation, generation);
    // A restarted round 4 must get its larger window, rather than immediately
    // signing NIL at the initial two-second budget again.
    let early = coordinator.drive(Vec::new(), now + std::time::Duration::from_secs(2));
    assert!(early.outbound.is_empty());
    assert_eq!(store.load().unwrap().unwrap().generation, generation);
    let expired = coordinator.drive(Vec::new(), now + std::time::Duration::from_secs(4));
    assert!(expired.outbound.iter().any(|message| matches!(message,
        BftNetworkMessage::Vote { statement, .. }
        if statement.round() == 4 && statement.phase() == BftPhase::Precommit && statement.value() == BftValue::Nil
    )));
    let generation = store.load().unwrap().unwrap().generation;
    let mut driver = BftDriver::new(signer, restarted, validators, selected.scope()).unwrap();
    driver.register_subject(&selected.subject()).unwrap();
    assert!(matches!(
        driver.accept_quorum_certificate(&forged),
        Err(BftDriverError::Bft(crate::BftError::InvalidSignature(_)))
    ));
    assert!(matches!(
        driver.accept_quorum_certificate(&proof),
        Err(BftDriverError::RoundMismatch {
            current: 4,
            actual: 0
        })
    ));
    assert_eq!(store.load().unwrap().unwrap().generation, generation);
    store.remove_files().unwrap();
    let _ = std::fs::remove_file(base.with_extension("lock"));
}
