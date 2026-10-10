#[path = "integration/support/mod.rs"]
mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use second::{
    BftConsensusEvent, NodeRuntime, Operation, SecondState, StateStore, ValidatorId,
    ValidatorRuntimeKeys,
};

use crate::support::{bind_validator_runtime, key, peer_record, temp_base, validator_set};

// Each simulated validator performs synchronous signature/persistence work.
// Give the shared harness one worker per node so it does not model 34 nodes on
// four executor threads and starve QUIC's finite I/O deadlines.
#[tokio::test(flavor = "multi_thread", worker_threads = 34)]
async fn thirty_four_validators_restore_candidates_and_finalize_private_business() {
    let set = validator_set(1, 1..=34);
    let recipient = support::account(340);
    let mut nodes = Vec::new();
    for id in 1_u64..=34 {
        let base = temp_base("large-validator-discovery");
        let store = StateStore::new(&base);
        store
            .initialize(&SecondState::genesis([recipient], 1), &set)
            .unwrap();
        let runtime = Arc::new(bind_validator_runtime(
            &store,
            ValidatorRuntimeKeys::new(
                ValidatorId::new(id),
                key((id * 3) as u8),
                key((id * 3 + 1) as u8),
            ),
            support::default_validator_runtime_config(),
        ));
        nodes.push((runtime, store, base));
    }
    let bootstrap = nodes[1..]
        .iter()
        .map(|(node, _, _)| peer_record(node))
        .collect::<Vec<_>>();
    let listeners = nodes[1..]
        .iter()
        .map(|(node, _, _)| support::spawn_node_runtime(node))
        .collect::<Vec<_>>();
    let first_task = support::spawn_node_runtime_with_bootstrap(&nodes[0].0, bootstrap);
    wait_for_all(&nodes[0].0, "initial discovery").await;
    first_task.abort();
    let _ = first_task.await;
    let (first, store, base) = nodes.remove(0);
    let node_id = first.node_id();
    drop(first);
    let restarted = Arc::new(bind_validator_runtime(
        &store,
        ValidatorRuntimeKeys::new(ValidatorId::new(1), key(3), key(4)),
        support::default_validator_runtime_config(),
    ));
    assert_eq!(restarted.node_id(), node_id);
    // No static bootstrap: every BFT endpoint must come from the persisted local cache.
    let restarted_task = support::spawn_node_runtime(&restarted);
    wait_for_all(&restarted, "restored discovery").await;
    for task in listeners {
        task.abort();
        let _ = task.await;
    }
    // Upgrade the same live nodes to complete exact-set connectivity, using the
    // restarted node's current endpoint rather than a stale bootstrap record.
    let mut records = vec![peer_record(&restarted)];
    records.extend(nodes.iter().map(|(node, _, _)| peer_record(node)));
    let workers = nodes
        .iter()
        .map(|(node, _, _)| support::spawn_node_runtime_with_bootstrap(node, records.clone()))
        .collect::<Vec<_>>();
    let connected = tokio::time::timeout(Duration::from_secs(60), async {
        while nodes
            .iter()
            .any(|(node, _, _)| node.connected_validator_ids().len() != 33)
        {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(
        connected.is_ok(),
        "incomplete large exact-set connectivity: {:?}",
        nodes
            .iter()
            .map(|(node, _, _)| (node.validator_id(), node.connected_validator_ids().len()))
            .collect::<Vec<_>>()
    );
    eprintln!("large-set exact connectivity established");
    let task = support::verified_task(
        3400,
        vec![Operation::Issue {
            account: recipient,
            count: 2,
        }],
    );
    // Only one member receives the private source; allocation and private pull
    // must converge through the production worker, followed by durable commit.
    restarted
        .submit_legal_task(task.signed_task().clone())
        .unwrap();
    let committed = tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            if support::progress::snapshots(
                std::iter::once(store.clone())
                    .chain(nodes.iter().map(|(_, store, _)| store.clone())),
            )
            .await
            .iter()
            .all(|snapshot| snapshot.state.task_succeeded(task.task_id()) == Some(true))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    restarted_task.abort();
    let _ = restarted_task.await;
    for worker in workers {
        worker.abort();
        let _ = worker.await;
    }
    let diagnostics = std::iter::once(&restarted)
        .chain(nodes.iter().map(|(node, _, _)| node))
        .map(|node| {
            (
                node.validator_id(),
                node.drain_bft_consensus_events().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    let failure_summary = diagnostics
        .iter()
        .map(|(id, events)| {
            (
                id,
                events.len(),
                events.iter().rev().take(3).collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();
    if committed.is_err() {
        for (node, disk_store, disk_base) in std::iter::once((&restarted, &store, &base))
            .chain(nodes.iter().map(|(node, store, base)| (node, store, base)))
        {
            let validator_id = node.validator_id().unwrap();
            let snapshot = disk_store.load().unwrap().unwrap();
            let scope = second::ConsensusScope::PreparedTask(task.task_id());
            let local = disk_store.bft_local_state(validator_id, &scope).unwrap();
            let local = local.as_ref().map(|state| {
                (
                    state.round(),
                    state.locked_round(),
                    state.locked_digest(),
                    state.valid_prevote_qc().map(|qc| qc.statement().round()),
                )
            });
            eprintln!(
                "large-set persisted validator={validator_id:?} base={} frontier={} prepared={} succeeded={:?} bft={local:?}",
                disk_base.display(),
                snapshot.state.next_currency_address(),
                second::PreparedTaskBook::new(disk_store.clone())
                    .unwrap()
                    .is_prepared(task.task_id()),
                snapshot.state.task_succeeded(task.task_id()),
            );
        }
    }
    assert!(
        committed.is_ok(),
        "large-set business did not converge: {failure_summary:?}"
    );
    eprintln!("large-set private issuance durably committed at all 34 nodes");
    let mut certified = false;
    for (_, events) in diagnostics {
        for event in events {
            if let BftConsensusEvent::CertifiedPreparedTask {
                task_id,
                certificate,
            } = event
            {
                assert_eq!(task_id, task.task_id());
                certificate.verify(&set).unwrap();
                assert!(certificate.votes().len() >= set.quorum_threshold());
                certified = true;
            }
        }
    }
    assert!(
        certified,
        "commit must have a valid large-set quorum certificate"
    );
    // Independent store instances force disk reload, beyond the workers' caches.
    let persisted = StateStore::new(&base).load().unwrap().unwrap();
    let expected = second::StateRecoveryPayload::from_persisted(&persisted)
        .unwrap()
        .encode_bytes()
        .unwrap();
    for disk_base in std::iter::once(&base).chain(nodes.iter().map(|(_, _, base)| base)) {
        let persisted = StateStore::new(disk_base).load().unwrap().unwrap();
        assert_eq!(persisted.state.balance(recipient), 2);
        assert_eq!(persisted.state.next_currency_address(), 3);
        assert_eq!(
            second::StateRecoveryPayload::from_persisted(&persisted)
                .unwrap()
                .encode_bytes()
                .unwrap(),
            expected
        );
    }
    drop(restarted);
    support::cleanup_node_runtime(store, base);
    for (node, store, base) in nodes {
        drop(node);
        support::cleanup_node_runtime(store, base);
    }
}

async fn wait_for_all(node: &NodeRuntime, stage: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let ids = node.connected_validator_ids();
        if ids.len() == 33 {
            assert!((2..=34).all(|id| ids.contains(&ValidatorId::new(id))));
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{stage}: only {} of 33 authenticated Validator connections; connected={ids:?}; events={:?}",
            ids.len(),
            node.drain_bft_consensus_events().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
