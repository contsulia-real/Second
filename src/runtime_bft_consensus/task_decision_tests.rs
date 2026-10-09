use super::*;
use crate::prepared::tests::{key, temp_store, validator_set};
use crate::{AuthorizerSet, LegalTaskPayload, Operation, PreparedTaskBook, SecondState};

#[test]
fn finality_deadline_retries_certified_business_without_another_message() {
    for received_certificate in [false, true] {
        let validators = validator_set();
        let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
        let account = crate::test_helpers::account(214);
        let task = crate::test_helpers::sign(
            LegalTaskPayload::new(
                TaskId::parse("interrupted-finality-commit").unwrap(),
                1,
                None,
                vec![Operation::RegisterAccount { account }],
            ),
            &key(9),
        )
        .unwrap()
        .verify(&authorizers)
        .unwrap();
        let (store, base) = temp_store();
        let mut state = SecondState::genesis([], 1);
        store.initialize(&state, &validators).unwrap();
        PreparedTaskBook::new(store.clone())
            .unwrap()
            .prepare(&mut state, &task, 1, &validators)
            .unwrap();
        let signer = ValidatorSigner::new(ValidatorId::new(1), key(4), store.clone());
        let target =
            ValidatorConsensusTarget::prepared_task(&store, ValidatorId::new(1), task.task_id())
                .unwrap();
        let scope = ConsensusScope::PreparedTask(task.task_id());
        let statement = target.finality_statement(&validators);
        let mut coordinator = BftConsensusCoordinator::new();
        let timeouts = BftTimeoutConfig::new(
            std::time::Duration::from_secs(1),
            std::time::Duration::from_secs(1),
            std::time::Duration::from_secs(1),
        );
        coordinator
            .register(signer, store.clone(), validators.clone(), target, timeouts)
            .unwrap();
        let now = Instant::now();
        let session = coordinator.sessions.get_mut(&scope).unwrap();
        // Inject the state left after collecting a quorum but before a failed
        // durable commit. No peer sends another vote or certificate.
        session.needs_start = false;
        session.bft_finality_ready = !received_certificate;
        session.deadline = Some(now + std::time::Duration::from_secs(1));
        let votes: BTreeMap<_, _> = (2..=4)
            .map(|id| {
                (
                    ValidatorId::new(id),
                    ValidatorVote::sign_unchecked(
                        &statement,
                        ValidatorId::new(id),
                        &key((id * 3 + 1) as u8),
                    ),
                )
            })
            .collect();
        let inbound = if received_certificate {
            vec![InboundBftMessage {
                validator_id: ValidatorId::new(2),
                message: BftNetworkMessage::FinalityCertificate {
                    scope: scope.clone(),
                    certificate: FinalityCertificate::new(
                        statement,
                        votes.into_values().collect(),
                        &validators,
                    )
                    .unwrap(),
                },
            }]
        } else {
            session.finality_votes = votes;
            session.deadline = Some(now);
            vec![]
        };
        let prepared = store.load_prepared_tasks().unwrap();
        store
            .replace_prepared_tasks(&prepared, &Default::default())
            .unwrap();
        let failed = coordinator.drive(inbound, now);
        assert!(failed.outbound.is_empty());
        assert!(coordinator.drain_events().iter().any(|event| matches!(
            event,
            BftConsensusEvent::Rejected {
                error: BftConsensusRuntimeError::Persistence(PersistenceError::StalePreparedTasks),
                ..
            }
        )));
        let retry = coordinator.next_deadline().unwrap();
        assert!(retry > now);
        assert!(
            coordinator.drive(vec![], now).outbound.is_empty(),
            "an interrupted commit must not spin on its expired deadline"
        );
        store
            .replace_prepared_tasks(&Default::default(), &prepared)
            .unwrap();
        let output = coordinator.drive(vec![], retry);
        assert!(
            coordinator
                .drain_events()
                .iter()
                .any(|event| matches!(event, BftConsensusEvent::CertifiedPreparedTask { .. }))
        );
        assert!(output.completed_prepared_tasks.contains(&task.task_id()));
        assert!(
            output
                .outbound
                .iter()
                .any(|message| matches!(message, BftNetworkMessage::FinalityCertificate { .. }))
        );
        let cold = crate::StateStore::new(&base).load().unwrap().unwrap();
        assert_eq!(cold.state.task_succeeded(task.task_id()), Some(true));
        assert!(cold.state.business.accounts.contains(&account));
        assert!(!cold.prepared_tasks.contains_key(&task.task_id()));
        store.remove_files().unwrap();
        let _ = std::fs::remove_file(base.with_extension("lock"));
    }
}

#[test]
fn disjoint_certified_choices_wait_for_commit_to_release_retained_alternative_claims() {
    let validators = validator_set();
    let alice = crate::test_helpers::account(111);
    let bob = crate::test_helpers::account(112);
    let source = crate::PaymentAddress::from_bytes([111; 32]);
    let destination = crate::PaymentAddress::from_bytes([112; 32]);
    let mut state = SecondState::genesis([alice, bob], 4);
    for (address, account) in [(source, alice), (destination, bob)] {
        state.business.payment_addresses.insert(
            address,
            crate::payment::PaymentAddressRecord {
                account,
                status: crate::PaymentAddressStatus::Active,
            },
        );
    }
    for number in 1..=3 {
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
    let waiting = task("crossed-a-waiting");
    let holder = task("crossed-b-holder");
    let (store, base) = temp_store();
    store.initialize(&state, &validators).unwrap();
    let mut book = PreparedTaskBook::new(store.clone()).unwrap();
    book.prepare(&mut state, &waiting, 1, &validators).unwrap();
    book.prepare(&mut state, &holder, 1, &validators).unwrap();
    let digest_for = |task_id: TaskId, number| {
        let mut plan = store
            .load_prepared_tasks()
            .unwrap()
            .remove(&task_id)
            .unwrap();
        let crate::prepared_plan::PreparedOperation::Transfer { currencies, .. } =
            &mut plan.operations[0]
        else {
            panic!("transfer fixture")
        };
        *currencies = vec![crate::CurrencyAddress::new(number)];
        plan.plan_digest().unwrap()
    };
    let holder_digest = digest_for(holder.task_id(), 3);
    assert!(
        book.admit_frozen_variant(
            &mut state,
            &holder,
            &validators,
            holder_digest,
            &[vec![crate::CurrencyAddress::new(3)]],
        )
        .unwrap()
        .is_none()
    );
    let waiting_digest = digest_for(waiting.task_id(), 2);
    let contention = book
        .admit_frozen_variant(
            &mut state,
            &waiting,
            &validators,
            waiting_digest,
            &[vec![crate::CurrencyAddress::new(2)]],
        )
        .unwrap()
        .unwrap();
    book.admit_contention(&mut state, &waiting, &validators, contention)
        .unwrap();
    let conflicting_digest = digest_for(waiting.task_id(), 3);
    let contention = book
        .admit_frozen_variant(
            &mut state,
            &waiting,
            &validators,
            conflicting_digest,
            &[vec![crate::CurrencyAddress::new(3)]],
        )
        .unwrap()
        .unwrap();
    book.admit_contention(&mut state, &waiting, &validators, contention)
        .unwrap();
    let signer = ValidatorSigner::new(ValidatorId::new(1), key(4), store.clone());
    let scope = ConsensusScope::PreparedTask(holder.task_id());
    let subject = BftProposalSubject::new(1, scope.clone(), holder_digest);
    let mut driver = BftDriver::new(
        signer.clone(),
        store.clone(),
        validators.clone(),
        scope.clone(),
    )
    .unwrap();
    driver.register_subject(&subject).unwrap();
    let statement = crate::BftStatement::new(
        1,
        scope.clone(),
        0,
        BftPhase::Prevote,
        BftValue::Digest(holder_digest),
    );
    let qc = BftQuorumCertificate::new(
        statement.clone(),
        (2..=4)
            .map(|id| {
                crate::BftVote::sign_unchecked(
                    &statement,
                    ValidatorId::new(id),
                    &key((id * 3 + 1) as u8),
                )
            })
            .collect(),
        &validators,
    )
    .unwrap();
    driver.accept_quorum_certificate(&qc).unwrap();
    store
        .reconcile_prepared_commit_qc(ValidatorId::new(1), &qc)
        .unwrap();
    let certify = |digest| {
        let statement = crate::FinalityStatement::new(1, 1, digest);
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
    let generation = store.load().unwrap().unwrap().generation;
    assert!(matches!(
        store.reconcile_prepared_commit_finality(
            ValidatorId::new(1),
            &waiting.task_id(),
            &certify(conflicting_digest),
        ),
        Err(crate::PersistenceError::InvalidSnapshot)
    ));
    assert_eq!(store.load().unwrap().unwrap().generation, generation);
    let certificate = certify(waiting_digest);
    assert!(
        store
            .reconcile_prepared_commit_finality(
                ValidatorId::new(1),
                &waiting.task_id(),
                &certificate
            )
            .unwrap()
            .is_empty()
    );
    let cold = crate::StateStore::new(&base).load().unwrap().unwrap();
    assert!(!cold.prepared_tasks[&holder.task_id()].conflict_abort);
    assert!(
        !cold.prepared_tasks[&waiting.task_id()]
            .candidate(waiting_digest)
            .unwrap()
            .unwrap()
            .commit_authorized
    );
    assert_eq!(
        PreparedTaskBook::new(store.clone())
            .unwrap()
            .claimed_currency_count(),
        3
    );
    let mut coordinator = BftConsensusCoordinator::new();
    coordinator
        .register(
            signer,
            store.clone(),
            validators.clone(),
            ValidatorConsensusTarget::PreparedTask {
                task_id: holder.task_id(),
                plan_digest: holder_digest,
            },
            BftTimeoutConfig::new(
                Duration::from_secs(2),
                Duration::from_secs(2),
                Duration::from_secs(2),
            ),
        )
        .unwrap();
    let output = coordinator.drive(
        vec![InboundBftMessage {
            validator_id: ValidatorId::new(2),
            message: BftNetworkMessage::FinalityCertificate {
                scope,
                certificate: certify(holder_digest),
            },
        }],
        Instant::now(),
    );
    assert!(output.retry_prepared_tasks.contains(&waiting.task_id()));
    let cold = crate::StateStore::new(&base).load().unwrap().unwrap();
    assert_eq!(cold.state.task_succeeded(holder.task_id()), Some(true));
    assert!(!cold.state.task_cancelled(holder.task_id()));
    let mut state = cold.state;
    let mut book = PreparedTaskBook::new(store.clone()).unwrap();
    assert_eq!(book.claimed_currency_count(), 1);
    assert!(
        book.admit_frozen_variant(
            &mut state,
            &waiting,
            &validators,
            waiting_digest,
            &[vec![crate::CurrencyAddress::new(2)]]
        )
        .unwrap()
        .is_none()
    );
    book.commit(&mut state, waiting.task_id(), &certificate)
        .unwrap();
    assert_eq!(state.balance(bob), 2);
    assert_eq!(
        state.business.currencies[&crate::CurrencyAddress::new(1)].owner,
        Some(alice)
    );
    assert_eq!(book.claimed_currency_count(), 0);
    store.remove_files().unwrap();
    let _ = std::fs::remove_file(base.with_extension("lock"));
}

#[test]
fn task_abort_recovers_finality_vote_and_certified_evidence_drives_all_holders_to_cancelled() {
    let validators = validator_set();
    let task_id = TaskId::parse("runtime-abort-recovery").unwrap();
    let authorizers = AuthorizerSet::new(
        crate::CURRENT_PROTOCOL_VERSION,
        [key(9).verifying_key().to_bytes()],
    )
    .unwrap();
    let task = crate::test_helpers::sign(
        LegalTaskPayload::new(
            task_id.clone(),
            crate::CURRENT_PROTOCOL_VERSION,
            None,
            vec![Operation::RegisterAccount {
                account: crate::test_helpers::account(101),
            }],
        ),
        &key(9),
    )
    .unwrap()
    .verify(&authorizers)
    .unwrap();
    let fixtures = (1..=4)
        .map(|_| {
            let (store, base) = temp_store();
            let mut state = SecondState::genesis([], 1);
            store.initialize(&state, &validators).unwrap();
            PreparedTaskBook::new(store.clone())
                .unwrap()
                .prepare(&mut state, &task, 1, &validators)
                .unwrap();
            (store, base)
        })
        .collect::<Vec<_>>();
    let abort = fixtures[0].0.prepared_abort_statement(&task_id).unwrap();
    let scope = ConsensusScope::PreparedTask(task_id.clone());
    let statement = crate::BftStatement::new(
        1,
        scope.clone(),
        0,
        BftPhase::Precommit,
        BftValue::Digest(abort.subject_digest()),
    );
    // This fixture represents a validated prior decision QC; it does not claim
    // to test initial conflict witness admission or four-node split resolution.
    let qc = BftQuorumCertificate::new(
        statement.clone(),
        (1..=3)
            .map(|id| {
                crate::BftVote::sign_unchecked(
                    &statement,
                    ValidatorId::new(id),
                    &key((id * 3 + 1) as u8),
                )
            })
            .collect(),
        &validators,
    )
    .unwrap();
    // A precommit QC's result takes priority over an older opposite prevote lock,
    // even if finality has not been signed yet. Test both outcome directions.
    let commit_digest = fixtures[0]
        .0
        .prepared_bft_proposal_subject(task_id.clone())
        .unwrap()
        .digest();
    for (first, decided) in [
        (commit_digest, abort.subject_digest()),
        (abort.subject_digest(), commit_digest),
    ] {
        let (store, base) = temp_store();
        let mut state = SecondState::genesis([], 1);
        store.initialize(&state, &validators).unwrap();
        PreparedTaskBook::new(store.clone())
            .unwrap()
            .prepare(&mut state, &task, 1, &validators)
            .unwrap();
        let signer = ValidatorSigner::new(ValidatorId::new(1), key(4), store.clone());
        let mut driver = BftDriver::new(
            signer.clone(),
            store.clone(),
            validators.clone(),
            scope.clone(),
        )
        .unwrap();
        driver
            .register_subject(&BftProposalSubject::new(1, scope.clone(), first))
            .unwrap();
        let prevote = crate::BftStatement::new(
            1,
            scope.clone(),
            0,
            BftPhase::Prevote,
            BftValue::Digest(first),
        );
        let proof = BftQuorumCertificate::new(
            prevote.clone(),
            (1..=3)
                .map(|id| {
                    crate::BftVote::sign_unchecked(
                        &prevote,
                        ValidatorId::new(id),
                        &key((id * 3 + 1) as u8),
                    )
                })
                .collect(),
            &validators,
        )
        .unwrap();
        driver.accept_quorum_certificate(&proof).unwrap();
        let ready = crate::BftStatement::new(
            1,
            scope.clone(),
            1,
            BftPhase::Precommit,
            BftValue::Digest(decided),
        );
        let proof = BftQuorumCertificate::new(
            ready.clone(),
            (2..=4)
                .map(|id| {
                    crate::BftVote::sign_unchecked(
                        &ready,
                        ValidatorId::new(id),
                        &key((id * 3 + 1) as u8),
                    )
                })
                .collect(),
            &validators,
        )
        .unwrap();
        store
            .accept_bft_precommit_qc(ValidatorId::new(1), &proof, &validators)
            .unwrap();
        let target =
            ValidatorConsensusTarget::prepared_task(&store, signer.validator_id(), task_id.clone())
                .unwrap();
        assert_eq!(
            target.finality_statement(&validators).subject_digest(),
            decided
        );
        let mut coordinator = BftConsensusCoordinator::new();
        coordinator
            .register(
                signer,
                store.clone(),
                validators.clone(),
                target,
                BftTimeoutConfig::new(
                    Duration::from_secs(2),
                    Duration::from_secs(2),
                    Duration::from_secs(2),
                ),
            )
            .unwrap();
        let output = coordinator.drive(Vec::new(), Instant::now());
        assert!(output.outbound.iter().any(|message| matches!(message,
            BftNetworkMessage::FinalityVote { statement, .. } if statement.subject_digest() == decided)));
        assert!(
            !output
                .outbound
                .iter()
                .any(|message| matches!(message, BftNetworkMessage::Proposal { .. }))
        );
        store.remove_files().unwrap();
        let _ = std::fs::remove_file(base.with_extension("lock"));
    }
    let signer = ValidatorSigner::new(ValidatorId::new(1), key(4), fixtures[0].0.clone());
    fixtures[0]
        .0
        .accept_bft_precommit_qc(ValidatorId::new(1), &qc, &validators)
        .unwrap();
    signer
        .sign_prepared_abort(task_id.clone(), &abort, &validators)
        .unwrap();

    let mut coordinators = fixtures
        .iter()
        .enumerate()
        .map(|(index, (_, base))| {
            let store = crate::StateStore::new(base);
            let signer = ValidatorSigner::new(
                ValidatorId::new(index as u64 + 1),
                key((index as u8 + 1) * 3 + 1),
                store.clone(),
            );
            let target = ValidatorConsensusTarget::prepared_task(
                &store,
                signer.validator_id(),
                task_id.clone(),
            )
            .unwrap();
            let mut coordinator = BftConsensusCoordinator::new();
            coordinator
                .register(
                    signer,
                    store,
                    validators.clone(),
                    target,
                    BftTimeoutConfig::new(
                        Duration::from_secs(2),
                        Duration::from_secs(2),
                        Duration::from_secs(2),
                    ),
                )
                .unwrap();
            coordinator
        })
        .collect::<Vec<_>>();
    let now = Instant::now();
    let mut messages = Vec::new();
    for (index, coordinator) in coordinators.iter_mut().enumerate() {
        let output = coordinator.drive(Vec::new(), now);
        if index == 0 {
            assert!(output.outbound.iter().any(|message| matches!(message, BftNetworkMessage::FinalityVote { statement, .. } if *statement == abort)));
            assert!(!output.outbound.iter().any(|message| matches!(
                message,
                BftNetworkMessage::Proposal { .. } | BftNetworkMessage::Vote { .. }
            )));
        }
        messages.extend(
            output
                .outbound
                .into_iter()
                .map(|message| InboundBftMessage {
                    validator_id: ValidatorId::new(index as u64 + 1),
                    message,
                }),
        );
    }
    // One signed finality vote and an invalid QC cannot admit an Abort candidate.
    let generation = fixtures[1].0.load().unwrap().unwrap().generation;
    let raw_vote = BftNetworkMessage::FinalityVote {
        scope: scope.clone(),
        statement: abort,
        vote: ValidatorVote::sign_unchecked(&abort, ValidatorId::new(1), &key(4)),
    };
    let forged = BftQuorumCertificate::from_untrusted_parts(
        statement.clone(),
        (1..=3)
            .map(|id| crate::BftVote::sign_unchecked(&statement, ValidatorId::new(id), &key(99)))
            .collect(),
    );
    coordinators[1].drive(
        vec![
            InboundBftMessage {
                validator_id: ValidatorId::new(1),
                message: raw_vote,
            },
            InboundBftMessage {
                validator_id: ValidatorId::new(1),
                message: BftNetworkMessage::QuorumCertificate(forged),
            },
        ],
        now,
    );
    assert!(
        !coordinators[1].sessions[&scope]
            .candidates
            .contains_key(&abort.subject_digest())
    );
    assert_eq!(
        fixtures[1].0.load().unwrap().unwrap().generation,
        generation
    );

    // Nodes 2 and 3 recover the certified candidate. Node 4 has only Commit
    // admission and learns cancellation exclusively from the final certificate.
    for (index, coordinator) in coordinators.iter_mut().enumerate().take(3).skip(1) {
        let output = coordinator.drive(
            vec![InboundBftMessage {
                validator_id: ValidatorId::new(1),
                message: BftNetworkMessage::QuorumCertificate(qc.clone()),
            }],
            now,
        );
        assert!(output.outbound.iter().any(|message| matches!(message, BftNetworkMessage::FinalityVote { statement, .. } if *statement == abort)));
        messages.extend(
            output
                .outbound
                .into_iter()
                .map(|message| InboundBftMessage {
                    validator_id: ValidatorId::new(index as u64 + 1),
                    message,
                }),
        );
    }
    let finality_votes = messages
        .into_iter()
        .filter(|message| matches!(message.message, BftNetworkMessage::FinalityVote { .. }))
        .collect::<Vec<_>>();
    let mut certificates = Vec::new();
    for (index, coordinator) in coordinators.iter_mut().enumerate().take(3) {
        let output = coordinator.drive(finality_votes.clone(), now);
        certificates.extend(
            output
                .outbound
                .into_iter()
                .filter(|message| matches!(message, BftNetworkMessage::FinalityCertificate { .. }))
                .map(|message| InboundBftMessage {
                    validator_id: ValidatorId::new(index as u64 + 1),
                    message,
                }),
        );
    }
    assert!(!certificates.is_empty());
    coordinators[3].drive(certificates, now);
    for (coordinator, (store, base)) in coordinators.iter_mut().zip(fixtures) {
        let snapshot = crate::StateStore::new(&base).load().unwrap().unwrap();
        assert!(snapshot.state.task_cancelled(task_id.clone()));
        assert!(snapshot.prepared_tasks.is_empty());
        assert!(snapshot.state.business.accounts.is_empty());
        assert!(coordinator.drain_events().iter().any(|event| matches!(event,
            BftConsensusEvent::CertifiedPreparedTask { certificate, .. } if certificate.statement() == abort)));
        store.remove_files().unwrap();
        let _ = std::fs::remove_file(base.with_extension("lock"));
    }
}
