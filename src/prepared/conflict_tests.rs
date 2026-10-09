use super::tests::{key, temp_store, validator_set};
use super::*;
use crate::currency::Currency;
use crate::payment::PaymentAddressRecord;
use crate::{AuthorizerSet, CurrencyAddress, CurrencyRole, LegalTaskPayload, PaymentAddressStatus};

#[tokio::test]
async fn frozen_conflict_validation_finds_all_actual_holders_without_mutating_or_trusting_invalid_sources()
 {
    let validators = validator_set();
    let alice = crate::test_helpers::account(71);
    let bob = crate::test_helpers::account(72);
    let new_account = crate::test_helpers::account(73);
    let source = PaymentAddress::from_bytes([71; 32]);
    let destination = PaymentAddress::from_bytes([72; 32]);
    let mut state = SecondState::genesis([alice, bob], 4);
    for (address, account) in [(source, alice), (destination, bob)] {
        state.business.payment_addresses.insert(
            address,
            PaymentAddressRecord {
                account,
                status: PaymentAddressStatus::Active,
            },
        );
    }
    for number in 1..=3 {
        let address = CurrencyAddress::new(number);
        state.business.currencies.insert(
            address,
            Currency {
                address,
                role: CurrencyRole::Circulation,
                owner: Some(if number == 3 { bob } else { alice }),
            },
        );
    }
    let authorizers = AuthorizerSet::new(
        CURRENT_PROTOCOL_VERSION,
        [key(9).verifying_key().to_bytes()],
    )
    .unwrap();
    let signed = |name, operations| {
        crate::test_helpers::sign(
            LegalTaskPayload::new(
                TaskId::parse(name).unwrap(),
                CURRENT_PROTOCOL_VERSION,
                Some(2),
                operations,
            ),
            &key(9),
        )
        .unwrap()
        .verify(&authorizers)
        .unwrap()
    };
    let transfer = Operation::Transfer {
        source,
        destination,
        amount: 1,
    };
    let candidate = signed(
        "conflict-candidate",
        vec![
            transfer.clone(),
            Operation::RegisterAccount {
                account: new_account,
            },
            Operation::RetirePaymentAddress {
                address: destination,
            },
        ],
    );
    let (remote, remote_base) = temp_store();
    remote.initialize(&state, &validators).unwrap();
    let mut remote_state = state.clone();
    let mut remote_book = PreparedTaskBook::new(remote.clone()).unwrap();
    remote_book
        .prepare(&mut remote_state, &candidate, 1, &validators)
        .unwrap();
    let digest = remote_book
        .prepared_plan_digest(candidate.task_id())
        .unwrap();

    let (local, local_base) = temp_store();
    local.initialize(&state, &validators).unwrap();
    let mut book = PreparedTaskBook::new(local.clone()).unwrap();
    let holders = [
        signed("holder-currency", vec![transfer.clone()]),
        signed(
            "holder-account",
            vec![Operation::RegisterAccount {
                account: new_account,
            }],
        ),
        signed(
            "holder-payment",
            vec![Operation::RetirePaymentAddress {
                address: destination,
            }],
        ),
    ];
    for holder in &holders {
        book.prepare(&mut state, holder, 1, &validators).unwrap();
    }
    let before = state.clone();
    let generation = local.load().unwrap().unwrap().generation;
    let validate = |task, digest, selection| {
        book.validate_resource_conflict(
            &state,
            task,
            1,
            &validators,
            digest,
            &[vec![CurrencyAddress::new(selection)]],
        )
    };
    let mut expected = holders
        .iter()
        .map(|task| task.task_id())
        .collect::<Vec<_>>();
    expected.sort();
    assert_eq!(validate(&candidate, digest, 1).unwrap(), expected);
    let own_digest = book.prepared_plan_digest(holders[0].task_id()).unwrap();
    assert!(matches!(
        validate(&holders[0], own_digest, 1),
        Err(PreparationError::InvalidPreparedPlan)
    ));
    // A valid intersection in the first operation cannot hide an invalid later one.
    let invalid = signed(
        "conflict-invalid",
        vec![
            transfer.clone(),
            Operation::RegisterAccount { account: alice },
        ],
    );
    assert!(matches!(
        validate(&invalid, digest, 1),
        Err(PreparationError::Execution(_))
    ));
    assert!(matches!(
        validate(&candidate, digest, 3),
        Err(PreparationError::Execution(
            ExecutionError::CurrencyNotOwned(_)
        ))
    ));
    assert!(matches!(
        validate(&candidate, digest, 2),
        Err(PreparationError::PreparedPlanDigestMismatch { .. })
    ));
    // Same payer/endpoints, but a distinct frozen coin: no resource conflict.
    let independent = signed("conflict-independent", vec![transfer]);
    remote_book
        .prepare(&mut remote_state, &independent, 1, &validators)
        .unwrap();
    let independent_digest = remote_book
        .prepared_plan_digest(independent.task_id())
        .unwrap();
    assert!(matches!(
        validate(&independent, independent_digest, 2),
        Err(PreparationError::InvalidPreparedPlan)
    ));
    // A terminal quorum proof may admit an independently valid body without
    // occupancy; ordinary conflict admission above still requires a blocker.
    let certify = |statement| {
        crate::FinalityCertificate::new(
            statement,
            (1..=3)
                .map(|id| {
                    crate::ValidatorVote::sign_unchecked(
                        &statement,
                        crate::ValidatorId::new(id),
                        &key((id * 3 + 1) as u8),
                    )
                })
                .collect(),
            &validators,
        )
        .unwrap()
    };
    let independent_certificate = certify(
        remote_book
            .prepared_finality_statement(independent.task_id())
            .unwrap(),
    );
    let certified = book
        .verify_certified_source(
            &state,
            &independent,
            1,
            &validators,
            &independent_certificate,
            &[vec![CurrencyAddress::new(2)]],
        )
        .unwrap();
    assert!(certified.blockers.is_empty());
    assert_eq!(
        certified.prepared.plan_digest().unwrap(),
        independent_digest
    );
    for statement in [
        crate::FinalityStatement::new(1, validators.version(), [0; 32]),
        crate::task_abort::statement(
            &independent.task_id(),
            independent.request_digest(),
            &validators,
        ),
    ] {
        assert!(
            book.verify_certified_source(
                &state,
                &independent,
                1,
                &validators,
                &certify(statement),
                &[vec![CurrencyAddress::new(2)]],
            )
            .is_err()
        );
    }
    assert!(state.same_persisted_state(&before));
    assert_eq!(local.load().unwrap().unwrap().generation, generation);
    assert_eq!(book.prepared_count(), 3);
    assert_eq!(book.claimed_currency_count(), 1);
    assert_eq!(state.bound_request_digest(candidate.task_id()), None);
    assert_eq!(state.bound_request_digest(invalid.task_id()), None);
    // Validation has not freed any original holder or granted Commit authority.
    let mut reloaded = PreparedTaskBook::new(local.clone()).unwrap();
    assert_eq!(reloaded.claimed_currency_count(), 1);
    assert!(matches!(
        reloaded.prepare_expected_plan(
            &mut state,
            &candidate,
            1,
            &validators,
            digest,
            &[vec![CurrencyAddress::new(1)]]
        ),
        Err(PreparationError::Claim(
            ClaimError::ExplicitCurrencyContention { .. }
        ))
    ));
    let contention = reloaded
        .verify_contention(
            &state,
            &candidate,
            1,
            &validators,
            digest,
            &[vec![CurrencyAddress::new(1)]],
        )
        .unwrap();
    let losers = reloaded
        .admit_contention(&mut state, &candidate, &validators, contention)
        .unwrap();
    assert_eq!(losers.len(), 3);
    let generation = local.load().unwrap().unwrap().generation;
    let restarted = PreparedTaskBook::new(local.clone()).unwrap();
    assert!(!restarted.is_prepared(candidate.task_id()));
    assert_eq!(restarted.claimed_currency_count(), 1);
    assert_eq!(restarted.prepared_count(), 3);
    let signer = crate::ValidatorSigner::new(crate::ValidatorId::new(1), key(4), local.clone());
    let scope = crate::ConsensusScope::PreparedTask(candidate.task_id());
    let subject = crate::BftProposalSubject::new(1, scope.clone(), digest);
    assert!(signer.sign_bft_proposal(&subject, 0, &validators).is_err());
    assert!(
        signer
            .sign_bft_prevote(scope, 0, crate::BftValue::Digest(digest), &validators, None)
            .is_err()
    );
    assert!(
        local
            .validator_set_for_prepared_task(&candidate.task_id(), digest)
            .is_err()
    );
    assert_eq!(local.load().unwrap().unwrap().generation, generation);
    // Repeated authenticated sources do not add a second durable write.
    let replay = reloaded
        .verify_contention(
            &state,
            &candidate,
            1,
            &validators,
            digest,
            &[vec![CurrencyAddress::new(1)]],
        )
        .unwrap();
    reloaded
        .admit_contention(&mut state, &candidate, &validators, replay)
        .unwrap();
    assert_eq!(local.load().unwrap().unwrap().generation, generation);
    // A quorum proof can protect a contender's priority, but is not a local
    // resource claim and cannot authorize a Commit signature before release.
    let qc_statement = crate::BftStatement::new(
        1,
        subject.scope().clone(),
        0,
        crate::BftPhase::Prevote,
        crate::BftValue::Digest(digest),
    );
    let qc = crate::BftQuorumCertificate::new(
        qc_statement.clone(),
        (1..=3)
            .map(|id| {
                crate::BftVote::sign_unchecked(
                    &qc_statement,
                    crate::ValidatorId::new(id),
                    &key((id * 3 + 1) as u8),
                )
            })
            .collect(),
        &validators,
    )
    .unwrap();
    let commit_statement = remote_book
        .prepared_finality_statement(candidate.task_id())
        .unwrap();
    let commit_certificate = crate::FinalityCertificate::new(
        commit_statement,
        (1..=3)
            .map(|id| {
                crate::ValidatorVote::sign_unchecked(
                    &commit_statement,
                    crate::ValidatorId::new(id),
                    &key((id * 3 + 1) as u8),
                )
            })
            .collect(),
        &validators,
    )
    .unwrap();
    assert!(matches!(
        reloaded.prepare_expected_plan(
            &mut state,
            &candidate,
            1,
            &validators,
            digest,
            &[vec![CurrencyAddress::new(1)]]
        ),
        Err(PreparationError::Claim(_))
    ));
    assert!(!reloaded.tasks[&candidate.task_id()].commit_authorized);
    assert_eq!(
        local
            .reconcile_prepared_commit_finality(
                crate::ValidatorId::new(3),
                &candidate.task_id(),
                &commit_certificate
            )
            .unwrap()
            .len(),
        3
    );
    let final_proof_generation = local.load().unwrap().unwrap().generation;
    local
        .reconcile_prepared_commit_finality(
            crate::ValidatorId::new(3),
            &candidate.task_id(),
            &commit_certificate,
        )
        .unwrap();
    assert_eq!(
        local.load().unwrap().unwrap().generation,
        final_proof_generation
    );
    assert!(
        local
            .bft_finality_ready(crate::ValidatorId::new(3), subject.scope(), digest)
            .unwrap()
    );
    assert!(!local.load().unwrap().unwrap().prepared_tasks[&candidate.task_id()].commit_authorized);
    assert!(
        crate::ValidatorSigner::new(crate::ValidatorId::new(3), key(10), local.clone())
            .sign_prepared_task(candidate.task_id(), &commit_statement, &validators)
            .is_err()
    );
    assert_eq!(
        local.load().unwrap().unwrap().generation,
        final_proof_generation
    );
    let runtime = crate::NodeRuntime::bind_loaded(
        "127.0.0.1:0".parse().unwrap(),
        &local,
        local.load().unwrap().unwrap(),
        crate::NodeRuntimeCapabilities::default().with_validator(
            crate::ValidatorRuntimeKeys::new(crate::ValidatorId::new(2), key(6), key(7)),
            crate::ValidatorRuntimeConfig::new(
                authorizers.clone(),
                crate::BftTimeoutConfig::new(
                    std::time::Duration::from_secs(2),
                    std::time::Duration::from_secs(2),
                    std::time::Duration::from_secs(2),
                ),
                || 1,
            ),
        ),
    )
    .unwrap();
    assert_eq!(state.task_succeeded(candidate.task_id()), Some(false));
    runtime
        .process_prepared_task_sync(vec![crate::runtime_bft::InboundBftMessage {
            validator_id: crate::ValidatorId::new(3),
            message: crate::BftNetworkMessage::QuorumCertificate(qc.clone()),
        }])
        .unwrap();
    assert_eq!(
        local
            .bft_local_state(crate::ValidatorId::new(2), subject.scope())
            .unwrap()
            .unwrap()
            .valid_prevote_qc(),
        Some(&qc)
    );
    assert!(!local.load().unwrap().unwrap().prepared_tasks[&candidate.task_id()].commit_authorized);
    drop(runtime);
    assert_eq!(
        local
            .reconcile_prepared_commit_qc(crate::ValidatorId::new(1), &qc)
            .unwrap()
            .len(),
        3
    );
    let observed_generation = local.load().unwrap().unwrap().generation;
    local
        .reconcile_prepared_commit_qc(crate::ValidatorId::new(1), &qc)
        .unwrap();
    assert!(signer.sign_bft_proposal(&subject, 0, &validators).is_err());
    assert!(
        signer
            .sign_bft_prevote(
                subject.scope().clone(),
                0,
                crate::BftValue::Digest(digest),
                &validators,
                None
            )
            .is_err()
    );
    assert_eq!(
        local.load().unwrap().unwrap().generation,
        observed_generation
    );
    reloaded = PreparedTaskBook::new(local.clone()).unwrap();
    state = local.load().unwrap().unwrap().state;
    // New expired requests remain rejected. A source admitted while valid can
    // acquire its exact frozen resources after the certified holders release.
    assert!(matches!(
        reloaded.verify_contention(
            &state,
            &independent,
            3,
            &validators,
            independent_digest,
            &[vec![CurrencyAddress::new(2)]]
        ),
        Err(PreparationError::Execution(ExecutionError::TaskExpired))
    ));
    for holder in &holders {
        let statement = local.prepared_abort_statement(&holder.task_id()).unwrap();
        let votes = (1..=3)
            .map(|id| {
                crate::ValidatorVote::sign_unchecked(
                    &statement,
                    crate::ValidatorId::new(id),
                    &key((id * 3 + 1) as u8),
                )
            })
            .collect();
        let certificate = crate::FinalityCertificate::new(statement, votes, &validators).unwrap();
        reloaded
            .abort_certified(&mut state, holder.task_id(), &certificate)
            .unwrap();
    }
    assert_eq!(
        reloaded
            .prepare_expected_plan(
                &mut state,
                &candidate,
                3,
                &validators,
                digest,
                &[vec![CurrencyAddress::new(1)]]
            )
            .unwrap(),
        PreparationOutcome::Prepared
    );
    assert!(reloaded.is_prepared(candidate.task_id()));
    assert_eq!(
        reloaded.prepared_plan_digest(candidate.task_id()).unwrap(),
        digest
    );
    for (store, base) in [(remote, remote_base), (local, local_base)] {
        store.remove_files().unwrap();
        let _ = std::fs::remove_file(base.with_extension("lock"));
    }
}
