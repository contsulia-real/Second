//! Missing historical variants must reuse the authenticated handoff, not old votes.
use crate::prepared::tests::{key, temp_store, validator_set};
use crate::*;
use std::time::Duration;

mod abort;
mod retiring;

#[tokio::test]
async fn certified_handoff_recovers_another_frozen_variant_without_old_votes() {
    let origin = validator_set();
    let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
    let alice = crate::test_helpers::account(191);
    let bob = crate::test_helpers::account(192);
    let source = PaymentAddress::from_bytes([191; 32]);
    let destination = PaymentAddress::from_bytes([192; 32]);
    let mut state = SecondState::genesis([alice, bob], 4);
    for (address, account) in [(source, alice), (destination, bob)] {
        state.business.payment_addresses.insert(
            address,
            crate::payment::PaymentAddressRecord {
                account,
                status: PaymentAddressStatus::Active,
            },
        );
    }
    for number in 1..=3 {
        let address = CurrencyAddress::new(number);
        state.business.currencies.insert(
            address,
            crate::currency::Currency {
                address,
                role: CurrencyRole::Circulation,
                owner: Some(alice),
            },
        );
    }
    let (provider, provider_base) = temp_store();
    provider.initialize(&state, &origin).unwrap();
    let task = crate::test_helpers::sign(
        LegalTaskPayload::new(
            TaskId::parse("inherited-frozen-variant").unwrap(),
            1,
            Some(2),
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
    .unwrap();
    let mut book = PreparedTaskBook::new(provider.clone()).unwrap();
    book.prepare(&mut state, &task, 1, &origin).unwrap();
    let first = provider.load_prepared_tasks().unwrap()[&task.task_id()].clone();
    let first_digest = first.plan_digest().unwrap();
    let mut second = first.clone();
    let crate::prepared_plan::PreparedOperation::Transfer { currencies, .. } =
        &mut second.operations[0]
    else {
        panic!("transfer fixture");
    };
    *currencies = vec![CurrencyAddress::new(2)];
    let second_digest = second.plan_digest().unwrap();
    let mut unlisted = first.clone();
    let crate::prepared_plan::PreparedOperation::Transfer { currencies, .. } =
        &mut unlisted.operations[0]
    else {
        panic!("transfer fixture");
    };
    *currencies = vec![CurrencyAddress::new(3)];
    assert!(
        book.admit_frozen_variant(
            &mut state,
            &task,
            &origin,
            second_digest,
            &[vec![CurrencyAddress::new(2)]],
        )
        .unwrap()
        .is_none()
    );
    let trusted = provider.load().unwrap().unwrap();
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
    let active = ValidatorSet::new(
        2,
        origin
            .credentials()
            .filter(|credential| credential.id() != ValidatorId::new(4))
            .cloned()
            .chain(std::iter::once(joining)),
    )
    .unwrap();
    let transition = provider
        .prepare_validator_set_transition(
            ValidatorSetTransition::new(
                1,
                &origin,
                &trusted.validator_registry,
                active.clone(),
                vec![admission],
                vec![],
                4,
            )
            .unwrap(),
        )
        .unwrap();
    let body = transition.handoff.as_ref().unwrap().encode().unwrap();
    let votes = |statement: FinalityStatement| {
        (1..=3)
            .map(|id| {
                ValidatorVote::sign_unchecked(
                    &statement,
                    ValidatorId::new(id),
                    &key((id * 3 + 1) as u8),
                )
            })
            .collect()
    };
    let certified = CertifiedValidatorSetTransition::new(
        transition.clone(),
        votes(transition.finality_statement()),
        &origin,
    )
    .unwrap();
    let proof = ValidatorSetTransitionProof::from_certified(&certified);
    let (target, target_base) = temp_store();
    target
        .install_validator_handoff_baseline(&proof, &body, &trusted, &authorizers)
        .unwrap();
    let mut current = target.load().unwrap().unwrap().state;
    let mut imported = PreparedTaskBook::new(target.clone()).unwrap();
    let later_account = crate::test_helpers::account(193);
    let later = crate::test_helpers::sign(
        LegalTaskPayload::new(
            TaskId::parse("after-variant-handoff").unwrap(),
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
    imported.prepare(&mut current, &later, 1, &active).unwrap();
    let statement = imported
        .prepared_finality_statement(later.task_id())
        .unwrap();
    imported
        .commit(
            &mut current,
            later.task_id(),
            &FinalityCertificate::new(statement, votes(statement), &active).unwrap(),
        )
        .unwrap();
    imported
        .prepare_expected_plan(
            &mut current,
            &task,
            3,
            &origin,
            first_digest,
            &[vec![CurrencyAddress::new(1)]],
        )
        .unwrap();
    let before = target.load().unwrap().unwrap();
    assert!(
        before.prepared_tasks[&task.task_id()]
            .candidate(second_digest)
            .unwrap()
            .is_none()
    );
    let node = NodeRuntime::bind_loaded(
        "127.0.0.1:0".parse().unwrap(),
        &target,
        before.clone(),
        NodeRuntimeCapabilities::default().with_validator(
            ValidatorRuntimeKeys::new(ValidatorId::new(5), key(15), key(16)),
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
    node.start_durable_prepared_consensus().unwrap();
    let scope = ConsensusScope::PreparedTask(task.task_id());
    assert!(matches!(
        node.install_fetched_prepared_task(
            1,
            &scope,
            unlisted.plan_digest().unwrap(),
            &unlisted.encode_source().unwrap(),
        ),
        Err(BftConsensusRuntimeError::Preparation(
            PreparationError::Persistence(PersistenceError::ValidatorRegistryMismatch)
        ))
    ));
    assert_eq!(
        target.load().unwrap().unwrap().generation,
        before.generation
    );
    let current_statement = FinalityStatement::new(1, 2, second_digest);
    assert!(
        node.process_prepared_task_sync(vec![crate::runtime_bft::InboundBftMessage {
            validator_id: ValidatorId::new(1),
            message: BftNetworkMessage::FinalityCertificate {
                scope: scope.clone(),
                certificate: FinalityCertificate::new(
                    current_statement,
                    votes(current_statement),
                    &active,
                )
                .unwrap(),
            },
        }])
        .unwrap()
        .is_empty()
    );
    assert_eq!(
        target.load().unwrap().unwrap().generation,
        before.generation
    );
    let statement = FinalityStatement::new(1, 1, second_digest);
    let invalid = FinalityCertificate::from_untrusted_parts(
        statement,
        vec![ValidatorVote::from_untrusted_parts(
            ValidatorId::new(1),
            [0; 64],
        )],
    );
    assert!(
        node.process_prepared_task_sync(vec![crate::runtime_bft::InboundBftMessage {
            validator_id: ValidatorId::new(1),
            message: BftNetworkMessage::FinalityCertificate {
                scope: scope.clone(),
                certificate: invalid,
            },
        }])
        .unwrap()
        .is_empty()
    );
    assert_eq!(
        target.load().unwrap().unwrap().generation,
        before.generation
    );
    let certificate = FinalityCertificate::new(statement, votes(statement), &origin).unwrap();
    let pass = node
        .process_prepared_task_sync(vec![crate::runtime_bft::InboundBftMessage {
            validator_id: ValidatorId::new(1),
            message: BftNetworkMessage::FinalityCertificate {
                scope,
                certificate: certificate.clone(),
            },
        }])
        .unwrap();
    assert!(
        pass.is_empty(),
        "exact inherited second candidate must recover and commit passively"
    );
    let cold = StateStore::new(&target_base).load().unwrap().unwrap();
    assert_eq!(
        cold.state.task_succeeded(task.task_id()),
        Some(true),
        "variant recovery events: {:?}",
        node.drain_bft_consensus_events().unwrap()
    );
    assert_eq!(
        cold.state.business.currencies[&CurrencyAddress::new(1)].owner,
        Some(alice)
    );
    assert_eq!(
        cold.state.business.currencies[&CurrencyAddress::new(2)].owner,
        Some(bob)
    );
    assert!(cold.state.business.accounts.contains(&later_account));
    assert_eq!(
        cold.state.business.currencies[&CurrencyAddress::new(3)].owner,
        Some(alice)
    );
    assert_eq!(
        cold.task_receipts[&task.task_id()].certificate().unwrap(),
        certificate
    );
    assert_eq!(
        cold.task_receipts[&task.task_id()]
            .plan()
            .plan_digest()
            .unwrap(),
        second_digest
    );
    assert_eq!(
        cold.state
            .protocol
            .task_handoff
            .as_ref()
            .unwrap()
            .encode()
            .unwrap(),
        body
    );
    assert_eq!(cold.validator_set, active);
    assert!(!cold.validator_safety_ready);
    assert!(cold.prepared_tasks.is_empty());
    assert!(cold.bft_local_states.is_empty());
    assert!(cold.validator_vote_locks.is_empty());
    let completed_source = cold.task_receipts[&task.task_id()].source().unwrap();
    node.install_fetched_prepared_task(
        1,
        &ConsensusScope::PreparedTask(task.task_id()),
        second_digest,
        &completed_source,
    )
    .unwrap();
    assert!(
        node.install_fetched_prepared_task(
            2,
            &ConsensusScope::PreparedTask(task.task_id()),
            second_digest,
            &completed_source,
        )
        .is_err(),
        "the active committee must not replace the completed source's original committee"
    );
    assert_eq!(target.load().unwrap().unwrap().generation, cold.generation);
    let bft = node.validator_bft.as_ref().unwrap();
    assert!(!bft.has_pending_prepared_task_sync());
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
    assert_eq!(node.drain_bft_consensus_events().unwrap().iter().filter(|event| matches!(event,
        BftConsensusEvent::CertifiedPreparedTask { task_id, .. } if *task_id == task.task_id()
    )).count(), 1);
    drop(node);
    for (store, base) in [(provider, provider_base), (target, target_base)] {
        store.remove_files().unwrap();
        let _ = std::fs::remove_file(base.with_extension("lock"));
    }
}
