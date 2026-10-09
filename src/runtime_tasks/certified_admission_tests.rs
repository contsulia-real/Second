use super::*;
use crate::prepared::tests::{key, temp_store, validator_set};
use crate::*;
use std::time::Duration;

#[tokio::test]
async fn certificate_before_blocked_variant_source_is_persisted_and_resumes_after_abort() {
    for owned_primary in [true, false] {
        run_buffered_certificate_admission(owned_primary).await;
    }
}

async fn run_buffered_certificate_admission(owned_primary: bool) {
    let validators = validator_set();
    let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
    let alice = crate::test_helpers::account(101);
    let bob = crate::test_helpers::account(102);
    let source = PaymentAddress::from_bytes([101; 32]);
    let destination = PaymentAddress::from_bytes([102; 32]);
    let mut state = SecondState::genesis([alice, bob], 3);
    for (address, account) in [(source, alice), (destination, bob)] {
        state.business.payment_addresses.insert(
            address,
            crate::payment::PaymentAddressRecord {
                account,
                status: PaymentAddressStatus::Active,
            },
        );
    }
    for number in 1..=2 {
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
    let a = task("buffered-finality-a");
    let b = task("buffered-finality-b");
    let c = task("buffered-finality-c");
    let (store, base) = temp_store();
    store.initialize(&state, &validators).unwrap();
    let mut book = PreparedTaskBook::new(store.clone()).unwrap();
    if owned_primary {
        book.prepare(&mut state, &a, 1, &validators).unwrap();
    } else {
        book.prepare(&mut state, &c, 1, &validators).unwrap();
    }
    book.prepare(&mut state, &b, 1, &validators).unwrap();
    if !owned_primary {
        let witness = book
            .verify_local_contention(&state, &a, 1, &validators)
            .unwrap();
        book.admit_contention(&mut state, &a, &validators, witness)
            .unwrap();
    }
    let mut alternative = store
        .load_prepared_tasks()
        .unwrap()
        .remove(&a.task_id())
        .unwrap();
    let crate::prepared_plan::PreparedOperation::Transfer { currencies, .. } =
        &mut alternative.operations[0]
    else {
        panic!("transfer fixture")
    };
    *currencies = vec![CurrencyAddress::new(2)];
    let digest = alternative.plan_digest().unwrap();
    let bytes = alternative.encode_source().unwrap();
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
    let certificate = certify(FinalityStatement::new(1, 1, digest));
    if !owned_primary {
        let snapshot = store.load().unwrap().unwrap();
        let (promoted, promoted_base) = temp_store();
        promoted.initialize(&snapshot.state, &validators).unwrap();
        promoted
            .replace_prepared_tasks(&Default::default(), &snapshot.prepared_tasks)
            .unwrap();
        let mut state = snapshot.state;
        let mut book = PreparedTaskBook::new(promoted.clone()).unwrap();
        let variant = book
            .admit_frozen_variant(
                &mut state,
                &a,
                &validators,
                digest,
                &[vec![CurrencyAddress::new(2)]],
            )
            .unwrap()
            .unwrap();
        book.admit_contention(&mut state, &a, &validators, variant)
            .unwrap();
        let abort = certify(promoted.prepared_abort_statement(&c.task_id()).unwrap());
        promoted
            .install_prepared_abort(&c.task_id(), &abort)
            .unwrap();
        state = promoted.load().unwrap().unwrap().state;
        book = PreparedTaskBook::new(promoted.clone()).unwrap();
        let primary = book.prepared_plan_digest(a.task_id()).unwrap();
        book.prepare_expected_plan(
            &mut state,
            &a,
            1,
            &validators,
            primary,
            &[vec![CurrencyAddress::new(1)]],
        )
        .unwrap();
        let cold = StateStore::new(&promoted_base).load().unwrap().unwrap();
        let plan = &cold.prepared_tasks[&a.task_id()];
        assert!(plan.commit_authorized);
        assert!(
            plan.candidate(digest)
                .unwrap()
                .is_some_and(|candidate| !candidate.commit_authorized)
        );
        assert_eq!(
            PreparedTaskBook::new(promoted.clone())
                .unwrap()
                .claimed_currency_count(),
            2
        );
        promoted.remove_files().unwrap();
        let _ = std::fs::remove_file(promoted_base.with_extension("lock"));
    }
    let scope = ConsensusScope::PreparedTask(a.task_id());
    let runtime = NodeRuntime::bind_loaded(
        "127.0.0.1:0".parse().unwrap(),
        &store,
        store.load().unwrap().unwrap(),
        NodeRuntimeCapabilities::default().with_validator(
            ValidatorRuntimeKeys::new(ValidatorId::new(1), key(3), key(4)),
            ValidatorRuntimeConfig::new(
                authorizers,
                BftTimeoutConfig::new(
                    Duration::from_secs(1),
                    Duration::from_secs(1),
                    Duration::from_secs(1),
                ),
                || 1,
            ),
        ),
    )
    .unwrap();
    runtime.start_prepared_task_consensus(a.task_id()).unwrap();
    let generation = store.load().unwrap().unwrap().generation;
    let inbound = runtime
        .process_prepared_task_sync(vec![InboundBftMessage {
            validator_id: ValidatorId::new(2),
            message: BftNetworkMessage::FinalityCertificate {
                scope: scope.clone(),
                certificate: certificate.clone(),
            },
        }])
        .unwrap();
    let bft = runtime.validator_bft.as_ref().unwrap();
    assert_eq!(store.load().unwrap().unwrap().generation, generation);
    bft.consensus().drive(inbound, tokio::time::Instant::now());
    assert!(
        bft.consensus()
            .pending_finality_certificate(&scope, digest)
            .is_some()
    );
    runtime
        .install_fetched_prepared_task(1, &scope, digest, &bytes)
        .unwrap();
    let cold = StateStore::new(&base).load().unwrap().unwrap();
    let selected = &cold.prepared_tasks[&a.task_id()];
    assert_eq!(
        selected.phase,
        crate::prepared_plan::PreparedTaskPhase::Finalized
    );
    assert_eq!(selected.plan_digest().unwrap(), digest);
    assert!(!selected.commit_authorized);
    assert_eq!(selected.has_owned_candidate(), owned_primary);
    assert_eq!(cold.state.balance(bob), 0);
    let generation = cold.generation;
    let mut trial = cold.state.clone();
    assert!(matches!(
        PreparedTaskBook::new(store.clone())
            .unwrap()
            .prepare_expected_plan(
                &mut trial,
                &a,
                1,
                &validators,
                digest,
                &[vec![CurrencyAddress::new(2)]]
            ),
        Err(PreparationError::AlreadyPrepared(_))
    ));
    assert_eq!(store.load().unwrap().unwrap().generation, generation);
    assert_eq!(
        PreparedTaskBook::new(store.clone())
            .unwrap()
            .claimed_currency_count(),
        2
    );
    let signer = ValidatorSigner::new(ValidatorId::new(1), key(4), store.clone());
    assert!(
        signer
            .sign_prepared_task(a.task_id(), &certificate.statement(), &validators)
            .is_err()
    );
    let abort = certify(store.prepared_abort_statement(&b.task_id()).unwrap());
    store.install_prepared_abort(&b.task_id(), &abort).unwrap();
    // No second delivery of A's certificate is needed after its blocker resolves.
    runtime.retry_contender(&a.task_id()).unwrap();
    let committed = StateStore::new(&base).load().unwrap().unwrap();
    assert_eq!(committed.state.task_succeeded(a.task_id()), Some(true));
    assert!(committed.state.task_cancelled(b.task_id()));
    assert_eq!(committed.state.balance(bob), 1);
    assert_eq!(
        committed.state.business.currencies[&CurrencyAddress::new(1)].owner,
        Some(alice)
    );
    assert_eq!(
        committed.task_receipts[&a.task_id()]
            .certificate()
            .unwrap()
            .statement(),
        certificate.statement()
    );
    assert_eq!(committed.prepared_tasks.len(), usize::from(!owned_primary));
    runtime
        .install_fetched_prepared_task(1, &scope, digest, &bytes)
        .unwrap();
    assert_eq!(
        store.load().unwrap().unwrap().generation,
        committed.generation,
        "an exact source arriving after certified completion must be a no-op"
    );
    let mut changed_selection = bytes.clone();
    let end = changed_selection.len();
    changed_selection[end - 8..].copy_from_slice(&CurrencyAddress::new(1).value().to_be_bytes());
    assert!(
        runtime
            .install_fetched_prepared_task(1, &scope, digest, &changed_selection)
            .is_err()
    );
    assert!(
        runtime
            .install_fetched_prepared_task(1, &scope, [27; 32], &bytes)
            .is_err()
    );
    assert_eq!(
        store.load().unwrap().unwrap().generation,
        committed.generation
    );
    let completed_events = runtime.drain_bft_consensus_events().unwrap();
    assert_eq!(
        completed_events
            .iter()
            .filter(|event| matches!(event,
        BftConsensusEvent::CertifiedPreparedTask { task_id, certificate: actual }
            if *task_id == a.task_id() && actual == &certificate))
            .count(),
        1,
        "a certified component committed outside its voting session must report completion"
    );
    runtime.retry_contender(&a.task_id()).unwrap();
    let completed_id = a.task_id();
    bft.consensus()
        .restore_completed_tasks_for(
            &committed,
            Duration::from_secs(1),
            std::iter::once(&completed_id),
            true,
        )
        .unwrap();
    assert!(!runtime.drain_bft_consensus_events().unwrap().iter().any(|event|
        matches!(event, BftConsensusEvent::CertifiedPreparedTask { task_id, .. } if *task_id == a.task_id())));
    let restored = crate::runtime_bft_consensus::ValidatorConsensusRuntime::new();
    restored
        .restore_completed_tasks(&committed, Duration::from_secs(1))
        .unwrap();
    assert!(
        restored.drain_events().is_empty(),
        "startup receipt replay must remain silent"
    );
    drop(runtime);
    store.remove_files().unwrap();
    let _ = std::fs::remove_file(base.with_extension("lock"));
}
