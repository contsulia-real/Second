//! A running node must consume membership installed by authenticated catch-up.
use super::*;
use crate::prepared::tests::{key, temp_store, validator_set};
use crate::{
    AuthorizerSet, LegalTaskPayload, NodeRuntimeCapabilities, Operation, PreparedTaskBook,
    SecondState, ValidatorRuntimeConfig, ValidatorRuntimeKeys,
};

#[tokio::test]
async fn running_node_resumes_business_after_external_certified_frontier_advance() {
    let validators =
        crate::ValidatorSet::new(1, validator_set().credentials().take(1).cloned()).unwrap();
    let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
    let account = crate::test_helpers::account(243);
    let (store, base) = temp_store();
    store
        .initialize(&SecondState::genesis([], 1), &validators)
        .unwrap();
    let request = |name: &str, operation| {
        crate::test_helpers::sign(
            LegalTaskPayload::new(TaskId::parse(name).unwrap(), 1, None, vec![operation]),
            &key(9),
        )
        .unwrap()
        .verify(&authorizers)
        .unwrap()
    };
    let warm = request(
        "frontier-runner-ready",
        Operation::RegisterAccount {
            account: crate::test_helpers::account(244),
        },
    );
    let issue = request(
        "external-certified-frontier",
        Operation::Issue { account, count: 1 },
    );
    let dependency = request(
        "frontier-runner-late-account",
        Operation::RegisterAccount { account },
    );
    let node = Arc::new(
        NodeRuntime::bind_loaded(
            "127.0.0.1:0".parse().unwrap(),
            &store,
            store.load().unwrap().unwrap(),
            NodeRuntimeCapabilities::default().with_validator(
                ValidatorRuntimeKeys::new(ValidatorId::new(1), key(3), key(4)),
                ValidatorRuntimeConfig::new(
                    authorizers,
                    crate::BftTimeoutConfig::new(
                        std::time::Duration::from_secs(1),
                        std::time::Duration::from_secs(1),
                        std::time::Duration::from_secs(1),
                    ),
                    || 1,
                ),
            ),
        )
        .unwrap(),
    );
    node.submit_legal_task(warm.signed_task().clone()).unwrap();
    let running = node.clone();
    let worker = tokio::spawn(async move {
        running.run(&[]).await.unwrap();
    });
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while store
            .load()
            .unwrap()
            .unwrap()
            .state
            .task_succeeded(warm.task_id())
            != Some(true)
        {
            assert!(!worker.is_finished());
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the original runner must finish business before external installation");
    store.queue_currency_allocation(&issue).unwrap();
    let allocation = crate::CurrencyAllocation::new(&issue, 1, 1).unwrap();
    let statement = allocation.finality_statement();
    let certificate = crate::FinalityCertificate::new(
        statement,
        vec![crate::ValidatorVote::sign_unchecked(
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
    node.validator_bft.as_ref().unwrap().consensus().wake();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !node
            .drain_bft_consensus_events()
            .unwrap()
            .iter()
            .any(|event| {
                matches!(
                    event,
                    crate::BftConsensusEvent::Rejected {
                        error: BftConsensusRuntimeError::Preparation(_),
                        ..
                    }
                )
            })
        {
            assert!(!worker.is_finished());
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("missing business dependency must reject preparation first");
    let waiting = store.load().unwrap().unwrap();
    assert!(!waiting.prepared_tasks.contains_key(&issue.task_id()));
    assert_eq!(
        waiting.state.protocol.task_bindings[&issue.task_id()]
            .allocation_task
            .as_ref(),
        Some(issue.signed_task())
    );
    node.submit_legal_task(dependency.signed_task().clone())
        .unwrap();
    let completed = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while store
            .load()
            .unwrap()
            .unwrap()
            .state
            .task_succeeded(issue.task_id())
            != Some(true)
        {
            assert!(!worker.is_finished());
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    worker.abort();
    let _ = worker.await;
    assert!(
        completed.is_ok(),
        "external certified frontier failed to resume its durable business"
    );
    let cold = crate::StateStore::new(&base).load().unwrap().unwrap();
    assert_eq!(cold.state.next_currency_address(), 2);
    assert_eq!(cold.state.balance(account), 1);
    assert_eq!(cold.state.task_succeeded(issue.task_id()), Some(true));
    assert!(cold.prepared_tasks.is_empty());
    assert_eq!(
        cold.task_receipts[&issue.task_id()].allocation().unwrap(),
        &(1, certificate)
    );
    drop(node);
    store.remove_files().unwrap();
    let _ = std::fs::remove_file(base.with_extension("lock"));
    let _ = std::fs::remove_file(crate::transport_identity_path(&base));
    let _ = std::fs::remove_file(base.with_extension("transport.lock"));
}

#[tokio::test]
async fn running_node_resumes_inherited_sources_after_external_membership_activation() {
    let validators = validator_set();
    let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
    let initial = SecondState::genesis([], 1);
    let (local, local_base) = temp_store();
    let (remote, remote_base) = temp_store();
    for store in [&local, &remote] {
        store.initialize(&initial, &validators).unwrap();
    }
    let task = |name, byte| {
        crate::test_helpers::sign(
            LegalTaskPayload::new(
                TaskId::parse(name).unwrap(),
                1,
                None,
                vec![Operation::RegisterAccount {
                    account: crate::test_helpers::account(byte),
                }],
            ),
            &key(9),
        )
        .unwrap()
        .verify(&authorizers)
        .unwrap()
    };
    let a = task("external-membership-local", 241);
    let b = task("external-membership-inherited", 242);
    PreparedTaskBook::new(local.clone())
        .unwrap()
        .prepare(&mut initial.clone(), &a, 1, &validators)
        .unwrap();
    let mut remote_state = initial.clone();
    let mut remote_book = PreparedTaskBook::new(remote.clone()).unwrap();
    for task in [&a, &b] {
        remote_book
            .prepare(&mut remote_state, task, 1, &validators)
            .unwrap();
    }
    let remote_snapshot = remote.load().unwrap().unwrap();
    let transition = remote
        .prepare_validator_set_transition(
            ValidatorSetTransition::new(
                1,
                &validators,
                &remote_snapshot.validator_registry,
                ValidatorSet::new(2, validators.credentials().cloned()).unwrap(),
                vec![],
                vec![],
                1,
            )
            .unwrap(),
        )
        .unwrap();
    let node = Arc::new(
        NodeRuntime::bind_loaded(
            "127.0.0.1:0".parse().unwrap(),
            &local,
            local.load().unwrap().unwrap(),
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
        .unwrap(),
    );
    let running = node.clone();
    let worker = tokio::spawn(async move { running.run(&[]).await });
    tokio::time::timeout(Duration::from_secs(3), async {
        while !local
            .load_shared()
            .unwrap()
            .unwrap()
            .bft_local_states
            .keys()
            .any(|(_, scope)| scope == &ConsensusScope::PreparedTask(a.task_id()))
        {
            assert!(!worker.is_finished());
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("consensus runner did not start");
    let collected = node.collect_validator_set_transition(transition).unwrap();
    assert!(!local.load_shared().unwrap().unwrap().prepared_tasks[&b.task_id()].commit_authorized);
    let statement = collected.finality_statement();
    let certified = CertifiedValidatorSetTransition::new(
        collected,
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
    .unwrap();
    // The same durable activation and wake used by membership_sync. There are
    // no peer votes that could produce output.validator_set_changed locally.
    local
        .activate_validator_set_transition_for_runtime(&certified, ValidatorId::new(1))
        .unwrap();
    node.validator_bft
        .as_ref()
        .unwrap()
        .refresh_authority()
        .unwrap();
    node.validator_bft.as_ref().unwrap().consensus().wake();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let snapshot = local.load_shared().unwrap().unwrap();
            if snapshot.prepared_tasks[&b.task_id()].commit_authorized
                && snapshot
                    .bft_local_states
                    .keys()
                    .any(|(_, scope)| scope == &ConsensusScope::PreparedTask(b.task_id()))
            {
                assert_eq!(snapshot.validator_set.version(), 2);
                assert_eq!(
                    snapshot.prepared_tasks[&b.task_id()].validator_set_version,
                    1
                );
                assert!(snapshot.state.business.accounts.is_empty());
                break;
            }
            assert!(!worker.is_finished());
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("membership catch-up did not resume the validated inherited task");
    worker.abort();
    let _ = worker.await;
    drop(node);
    for (store, base) in [(local, local_base), (remote, remote_base)] {
        store.remove_files().unwrap();
        let _ = std::fs::remove_file(base.with_extension("lock"));
    }
}
