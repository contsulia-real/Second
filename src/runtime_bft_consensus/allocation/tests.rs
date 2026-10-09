//! A concurrent preparation must not discard a certified allocation's retry source.
use super::*;
use crate::prepared::tests::{key, temp_store, validator_set};
use crate::*;

#[tokio::test]
async fn allocated_task_resumes_after_another_task_changes_its_snapshot() {
    let validators = ValidatorSet::new(1, validator_set().credentials().take(1).cloned()).unwrap();
    let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
    let alice = crate::test_helpers::account(61);
    let (store, base) = temp_store();
    store
        .initialize(&SecondState::genesis([alice], 1), &validators)
        .unwrap();
    let sign = |name, operation| {
        crate::test_helpers::sign(
            LegalTaskPayload::new(TaskId::parse(name).unwrap(), 1, None, vec![operation]),
            &key(9),
        )
        .unwrap()
        .verify(&authorizers)
        .unwrap()
    };
    let issue = sign(
        "allocated-stale-source",
        Operation::Issue {
            account: alice,
            count: 1,
        },
    );
    store.queue_currency_allocation(&issue).unwrap();
    let allocation = CurrencyAllocation::new(&issue, 1, 1).unwrap();
    let certificate = FinalityCertificate::new(
        allocation.finality_statement(),
        vec![ValidatorVote::sign_unchecked(
            &allocation.finality_statement(),
            ValidatorId::new(1),
            &key(4),
        )],
        &validators,
    )
    .unwrap();
    store
        .install_currency_allocation(&allocation, &certificate)
        .unwrap();
    let stale = store.load_shared().unwrap().unwrap();
    let node = NodeRuntime::bind_loaded(
        "127.0.0.1:0".parse().unwrap(),
        &store,
        (*stale).clone(),
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
    .unwrap();
    let other = sign(
        "concurrent-allocated-preparation",
        Operation::RegisterAccount {
            account: crate::test_helpers::account(62),
        },
    );
    PreparedTaskBook::new(store.clone())
        .unwrap()
        .prepare(&mut stale.state.clone(), &other, 1, &validators)
        .unwrap();
    node.resume_allocation_task(&issue, stale).unwrap();
    let cold = StateStore::new(&base).load().unwrap().unwrap();
    assert!(
        cold.prepared_tasks.contains_key(&issue.task_id()),
        "a stale snapshot discarded the certified allocation instead of preparing it"
    );
    assert!(cold.prepared_tasks.contains_key(&other.task_id()));
    let binding = &cold.state.protocol.task_bindings[&issue.task_id()];
    assert_eq!(binding.allocation, Some((1, 1)));
    assert!(binding.allocation_certificate.is_some());
    assert!(binding.allocation_task.is_none());
    assert_eq!(cold.state.next_currency_address(), 2);
    assert_eq!(cold.state.balance(alice), 0);
    let subject = node
        .validator_bft
        .as_ref()
        .unwrap()
        .consensus()
        .coordinator
        .lock()
        .unwrap()
        .sessions[&ConsensusScope::PreparedTask(issue.task_id())]
        .subject
        .clone();
    assert_eq!(
        subject.digest(),
        cold.prepared_tasks[&issue.task_id()].plan_digest().unwrap()
    );

    let queued = sign(
        "unallocated-stale-frontier",
        Operation::Issue {
            account: alice,
            count: 1,
        },
    );
    store.queue_currency_allocation(&queued).unwrap();
    let stale = store.load_shared().unwrap().unwrap();
    let advancing = sign(
        "concurrent-frontier-advance",
        Operation::Issue {
            account: alice,
            count: 1,
        },
    );
    let allocation = CurrencyAllocation::new(&advancing, 1, 2).unwrap();
    let statement = allocation.finality_statement();
    let certificate = FinalityCertificate::new(
        statement,
        vec![ValidatorVote::sign_unchecked(
            &statement,
            ValidatorId::new(1),
            &key(4),
        )],
        &validators,
    )
    .unwrap();
    store
        .install_currency_allocation(&allocation, &certificate)
        .unwrap();
    node.resume_allocation_task(&queued, stale).unwrap();
    let current = store.load_shared().unwrap().unwrap();
    assert_eq!(current.state.next_currency_address(), 3);
    assert!(
        current.state.protocol.task_bindings[&queued.task_id()]
            .allocation
            .is_none()
    );
    let allocation = CurrencyAllocation::new(&queued, 1, 3).unwrap();
    assert!(
        node.validator_bft
            .as_ref()
            .unwrap()
            .consensus()
            .has_allocation_candidate(&allocation.scope(), allocation.digest())
    );
    // A certified range may arrive before a business dependency. Rejection
    // cannot discard its signature or consume a second range on later retry.
    let bob = crate::test_helpers::account(63);
    let waiting = sign(
        "allocated-waiting-for-account",
        Operation::Issue {
            account: bob,
            count: 1,
        },
    );
    store.queue_currency_allocation(&waiting).unwrap();
    let allocation = CurrencyAllocation::new(&waiting, 1, 3).unwrap();
    let statement = allocation.finality_statement();
    let certificate = FinalityCertificate::new(
        statement,
        vec![ValidatorVote::sign_unchecked(
            &statement,
            ValidatorId::new(1),
            &key(4),
        )],
        &validators,
    )
    .unwrap();
    store
        .install_currency_allocation(&allocation, &certificate)
        .unwrap();
    node.resume_allocation_task(&waiting, store.load_shared().unwrap().unwrap())
        .unwrap();
    let rejected = StateStore::new(&base).load().unwrap().unwrap();
    assert!(!rejected.prepared_tasks.contains_key(&waiting.task_id()));
    let binding = &rejected.state.protocol.task_bindings[&waiting.task_id()];
    assert_eq!(
        binding.allocation_task.as_ref(),
        Some(waiting.signed_task())
    );
    assert_eq!(binding.allocation_certificate.as_ref(), Some(&certificate));
    assert_eq!(binding.allocation, Some((3, 1)));
    let registering = sign(
        "allocated-late-account",
        Operation::RegisterAccount { account: bob },
    );
    let mut book = PreparedTaskBook::new(store.clone()).unwrap();
    book.prepare(&mut rejected.state.clone(), &registering, 1, &validators)
        .unwrap();
    let statement = book
        .prepared_finality_statement(registering.task_id())
        .unwrap();
    let certificate = FinalityCertificate::new(
        statement,
        vec![ValidatorVote::sign_unchecked(
            &statement,
            ValidatorId::new(1),
            &key(4),
        )],
        &validators,
    )
    .unwrap();
    store
        .finalize_prepared_task(
            &registering.task_id(),
            statement.subject_digest(),
            &certificate,
        )
        .unwrap();
    node.retry_contender(&registering.task_id()).unwrap();
    let retried = StateStore::new(&base).load().unwrap().unwrap();
    assert_eq!(
        retried.prepared_tasks[&waiting.task_id()].source_task,
        *waiting.signed_task()
    );
    assert_eq!(
        retried.state.protocol.task_bindings[&waiting.task_id()].allocation,
        Some((3, 1))
    );
    assert!(
        retried.state.protocol.task_bindings[&waiting.task_id()]
            .allocation_task
            .is_none()
    );
    assert_eq!(retried.state.next_currency_address(), 4);
    drop(node);
    store.remove_files().unwrap();
    let _ = std::fs::remove_file(base.with_extension("lock"));
    let _ = std::fs::remove_file(crate::transport_identity_path(&base));
    let _ = std::fs::remove_file(base.with_extension("transport.lock"));
}
