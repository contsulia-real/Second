//! A prior validated witness may seek Abort, never acquire invalid business rights.
use super::*;
use crate::runtime_bft::InboundBftMessage;

#[tokio::test]
async fn retired_address_witness_requires_exact_source_and_preserves_commit_proofs() {
    let validators = validator_set();
    let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
    let alice = crate::test_helpers::account(221);
    let bob = crate::test_helpers::account(222);
    let source_address = PaymentAddress::from_bytes([221; 32]);
    let destination = PaymentAddress::from_bytes([222; 32]);
    let mut initial = SecondState::genesis([alice, bob], 3);
    for (address, account) in [(source_address, alice), (destination, bob)] {
        initial.business.payment_addresses.insert(
            address,
            crate::payment::PaymentAddressRecord {
                account,
                status: PaymentAddressStatus::Active,
            },
        );
    }
    for number in 1..=2 {
        let address = CurrencyAddress::new(number);
        initial.business.currencies.insert(
            address,
            crate::currency::Currency {
                address,
                role: CurrencyRole::Circulation,
                owner: Some(alice),
            },
        );
    }
    let sign = |name: &str, operations| {
        crate::test_helpers::sign(
            LegalTaskPayload::new(TaskId::parse(name).unwrap(), 1, None, operations),
            &key(9),
        )
        .unwrap()
        .verify(&authorizers)
        .unwrap()
    };
    let transfer = sign(
        "unusable-known-transfer",
        vec![Operation::Transfer {
            source: source_address,
            destination,
            amount: 1,
        }],
    );
    let retire = sign(
        "unusable-address-retirement",
        vec![Operation::RetirePaymentAddress {
            address: destination,
        }],
    );
    let (provider, _) = temp_store();
    provider.initialize(&initial, &validators).unwrap();
    PreparedTaskBook::new(provider.clone())
        .unwrap()
        .prepare(&mut initial.clone(), &transfer, 1, &validators)
        .unwrap();
    let mut witness = provider
        .load_prepared_tasks()
        .unwrap()
        .remove(&transfer.task_id())
        .unwrap();
    let digest = witness.plan_digest().unwrap();
    let body = witness.encode_source().unwrap();
    witness.commit_authorized = false;
    let (store, base) = temp_store();
    let mut state = initial;
    state.bind_task(&transfer).unwrap();
    store.initialize(&state, &validators).unwrap();
    store
        .replace_prepared_tasks(
            &Default::default(),
            &std::collections::BTreeMap::from([(transfer.task_id(), witness)]),
        )
        .unwrap();
    let mut book = PreparedTaskBook::new(store.clone()).unwrap();
    book.prepare(&mut state, &retire, 1, &validators).unwrap();
    let statement = book.prepared_finality_statement(retire.task_id()).unwrap();
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
        &validators,
    )
    .unwrap();
    store
        .finalize_prepared_task(
            &retire.task_id(),
            certificate.statement().subject_digest(),
            &certificate,
        )
        .unwrap();
    // Recovery can execute an already-certified retirement without another
    // finality message. Its business event must still wake the old transfer.
    let recovered =
        PreparedTaskBook::commit_certified_component(&store, &retire.task_id(), None).unwrap();
    assert!(recovered.completed.contains(&retire.task_id()));
    assert!(recovered.retry.contains(&transfer.task_id()));
    let before = store.load().unwrap().unwrap();
    let mut owner_state = provider.load().unwrap().unwrap().state;
    let mut owner_book = PreparedTaskBook::new(provider.clone()).unwrap();
    owner_book
        .prepare(&mut owner_state, &retire, 1, &validators)
        .unwrap();
    assert!(
        provider
            .contenders_for(&retire.task_id())
            .unwrap()
            .contains(&transfer.task_id())
    );
    owner_book
        .commit(&mut owner_state, retire.task_id(), &certificate)
        .unwrap();
    let owner_before = provider.load().unwrap().unwrap();
    let bind = |store: &StateStore| {
        NodeRuntime::bind_loaded(
            "127.0.0.1:0".parse().unwrap(),
            store,
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
                    || 1,
                ),
            ),
        )
        .unwrap()
    };
    let runtime = bind(&store);
    let scope = ConsensusScope::PreparedTask(transfer.task_id());
    let mut wrong_selection = body.clone();
    *wrong_selection.last_mut().unwrap() = 2;
    assert!(
        runtime
            .install_fetched_prepared_task(1, &scope, digest, &wrong_selection)
            .is_err()
    );
    assert_eq!(store.load().unwrap().unwrap().generation, before.generation);
    // Both retained and not-yet-admitted complete Commit proofs outrank Abort.
    for (protected_before, owned) in [(&before, false), (&owner_before, true)] {
        // A QC signs its task scope even before its frozen variant is known.
        for proof_digest in [digest, [77; 32]] {
            for phase in [BftPhase::Prevote, BftPhase::Precommit] {
                let (protected, _) = temp_store();
                protected
                    .initialize(&protected_before.state, &validators)
                    .unwrap();
                protected
                    .replace_prepared_tasks(&Default::default(), &protected_before.prepared_tasks)
                    .unwrap();
                let protected_runtime = bind(&protected);
                let statement =
                    BftStatement::new(1, scope.clone(), 0, phase, BftValue::Digest(proof_digest));
                let qc = BftQuorumCertificate::new(
                    statement.clone(),
                    (2..=4)
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
                protected_runtime
                    .validator_bft
                    .as_ref()
                    .unwrap()
                    .consensus()
                    .drive(
                        vec![InboundBftMessage {
                            validator_id: ValidatorId::new(2),
                            message: BftNetworkMessage::QuorumCertificate(qc),
                        }],
                        tokio::time::Instant::now(),
                    );
                let generation = protected.load().unwrap().unwrap().generation;
                if owned {
                    protected_runtime
                        .retry_contender(&transfer.task_id())
                        .unwrap();
                } else {
                    assert!(
                        protected_runtime
                            .install_fetched_prepared_task(1, &scope, digest, &body)
                            .is_err()
                    );
                }
                let cold = protected.load().unwrap().unwrap();
                assert_eq!(cold.generation, generation);
                assert!(!cold.prepared_tasks[&transfer.task_id()].conflict_abort);
                drop(protected_runtime);
                protected.remove_files().unwrap();
            }
        }
    }
    let owner_runtime = bind(&provider);
    owner_runtime.retry_contender(&transfer.task_id()).unwrap();
    let owner_cold = provider.load().unwrap().unwrap();
    assert!(owner_cold.prepared_tasks[&transfer.task_id()].conflict_abort);
    assert!(owner_cold.prepared_tasks[&transfer.task_id()].has_owned_candidate());
    assert_eq!(owner_cold.state.payment_execution_count(), 1);
    assert!(!owner_cold.state.task_cancelled(transfer.task_id()));
    assert!(owner_cold.validator_vote_locks.is_empty());
    let generation = owner_cold.generation;
    owner_runtime.retry_contender(&transfer.task_id()).unwrap();
    assert_eq!(provider.load().unwrap().unwrap().generation, generation);
    drop(owner_runtime);
    runtime
        .install_fetched_prepared_task(1, &scope, digest, &body)
        .unwrap();
    let cold = StateStore::new(&base).load().unwrap().unwrap();
    assert!(cold.prepared_tasks[&transfer.task_id()].conflict_abort);
    assert!(!cold.prepared_tasks[&transfer.task_id()].has_owned_candidate());
    assert_eq!(cold.state.payment_execution_count(), 0);
    assert_eq!(cold.state.balance(alice), 2);
    assert_eq!(cold.state.balance(bob), 0);
    assert!(!cold.state.task_cancelled(transfer.task_id()));
    assert!(cold.validator_vote_locks.is_empty());
    let generation = cold.generation;
    runtime
        .install_fetched_prepared_task(1, &scope, digest, &body)
        .unwrap();
    assert_eq!(store.load().unwrap().unwrap().generation, generation);
    drop(runtime);
    provider.remove_files().unwrap();
    store.remove_files().unwrap();
}
