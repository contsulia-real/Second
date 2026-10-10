use crate::support::{self, bind_validator_runtime, key, peer_record, temp_base, validator_set};
use second::*;
use std::sync::Arc;
use std::time::Duration;

async fn publish(client: &QuicClient, record: &PeerRecord) -> RemotePublicCheckpointSubmission {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let peer = client.connect(record.address()).await.unwrap();
            let result =
                client_submit_public_checkpoint(&peer, ValidatorId::new(1), &key(3), 1).await;
            peer.close();
            match result {
                Ok(accepted) => break accepted,
                Err(NetworkError::GovernanceRejected(GovernanceRejection::Busy)) => {
                    tokio::time::sleep(Duration::from_millis(50)).await
                }
                other => panic!("public checkpoint submission failed: {other:?}"),
            }
        }
    })
    .await
    .expect("validator peers did not become ready")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_operator_publishes_quorum_proofs_and_running_public_node_tracks_changes() {
    let validators = validator_set(1, 1..=4);
    let recipient = support::account(90);
    let mut fixtures = Vec::new();
    for id in 1..=4 {
        let base = temp_base(&format!("public-checkpoint-publisher-{id}"));
        let store = StateStore::new(&base);
        store
            .initialize(&SecondState::genesis([recipient], 1), &validators)
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
        fixtures.push((runtime, store, base));
    }
    let records = fixtures
        .iter()
        .map(|(runtime, _, _)| peer_record(runtime))
        .collect::<Vec<_>>();
    let mut workers = fixtures
        .iter()
        .enumerate()
        .take(3)
        .map(|(index, (runtime, _, _))| {
            support::spawn_node_runtime_with_bootstrap(
                runtime,
                records
                    .iter()
                    .enumerate()
                    .filter(|(other, _)| *other != index)
                    .map(|(_, record)| record.clone())
                    .collect(),
            )
        })
        .collect::<Vec<_>>();
    let client = QuicClient::new(
        "127.0.0.1:0".parse().unwrap(),
        records[0].certificate_der(),
        QuicTransportIdentity::generate().unwrap(),
    )
    .unwrap();
    let public_base = temp_base("public-checkpoint-observer");
    let public_store = PublicStateStore::new(&public_base);
    public_store
        .initialize(
            validators.clone(),
            ValidatorRegistry::from_validator_set(&validators).unwrap(),
        )
        .unwrap();
    let public = Arc::new(
        NodeRuntime::load_public_and_bind("127.0.0.1:0".parse().unwrap(), &public_store).unwrap(),
    );
    workers.push(support::spawn_node_runtime_with_bootstrap(
        &public,
        vec![records[0].clone()],
    ));
    let mut stage = "first publication";
    let mut business_task = None;
    let result = tokio::time::timeout(Duration::from_secs(60), async {
        let first = publish(&client, &records[0]).await;
        assert_eq!(first.epoch, 1);
        stage = "first proof propagation";
        loop {
            if support::progress::snapshots(
                fixtures.iter().take(3).map(|(_, store, _)| store.clone()),
            )
            .await
            .iter()
            .all(|snapshot| snapshot.public_checkpoint_proof.is_some())
                && support::progress::public_snapshot(&public_store)
                    .await
                    .checkpoint_proof
                    .is_some()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        stage = "late validator connection";
        workers.push(support::spawn_node_runtime_with_bootstrap(
            &fixtures[3].0,
            records[..3].to_vec(),
        ));
        while fixtures[3].0.connected_validator_ids().len() < 3 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        stage = "late validator proof catchup";
        // The authenticated connection itself must catch up durable evidence;
        // another operator publication is not a recovery trigger.
        while support::progress::snapshot(&fixtures[3].1)
            .await
            .public_checkpoint_proof
            .is_none()
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let generation = fixtures[0].1.load().unwrap().unwrap().generation;
        assert_eq!(publish(&client, &records[0]).await, first);
        assert_eq!(
            fixtures[0].1.load().unwrap().unwrap().generation,
            generation
        );
        let unauthorized = client.connect(records[0].address()).await.unwrap();
        let rejected =
            client_submit_public_checkpoint(&unauthorized, ValidatorId::new(1), &key(4), 1).await;
        assert!(
            matches!(
                rejected,
                Err(NetworkError::GovernanceRejected(
                    GovernanceRejection::Unauthorized
                ))
            ),
            "unexpected identity rejection: {rejected:?}"
        );
        unauthorized.close();
        stage = "business task propagation";
        let task = support::verified_task(
            1920,
            vec![Operation::Issue {
                account: recipient,
                count: 2,
            }],
        );
        business_task = Some(task.task_id());
        fixtures[0]
            .0
            .submit_legal_task(task.signed_task().clone())
            .unwrap();
        loop {
            if support::progress::snapshots(fixtures.iter().map(|(_, store, _)| store.clone()))
                .await
                .iter()
                .all(|snapshot| snapshot.state.task_succeeded(task.task_id()) == Some(true))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        stage = "second publication";
        let second = publish(&client, &records[0]).await;
        assert_eq!(second.epoch, 2);
        assert_ne!(second.checkpoint_digest, first.checkpoint_digest);
        stage = "second proof propagation";
        loop {
            if support::progress::snapshots(fixtures.iter().map(|(_, store, _)| store.clone()))
                .await
                .iter()
                .all(|snapshot| {
                    snapshot
                        .public_checkpoint_proof
                        .as_ref()
                        .is_some_and(|proof| proof.checkpoint().epoch() == 2)
                })
                && support::progress::public_snapshot(&public_store)
                    .await
                    .checkpoint_proof
                    .as_ref()
                    .is_some_and(|proof| proof.checkpoint().epoch() == 2)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        for (_, store, _) in &fixtures {
            let state = support::progress::snapshot(store).await;
            let certified = state
                .public_checkpoint_proof
                .unwrap()
                .verify_checkpoint(&validators)
                .unwrap();
            assert_eq!(
                certified.checkpoint().summary(),
                &state.state.public_currency_summary()
            );
            assert!(state.latest_public_delta.is_some());
        }
        assert_eq!(
            support::progress::public_snapshot(&public_store)
                .await
                .view
                .unwrap()
                .summary
                .current_supply,
            2
        );
    })
    .await;
    let failure = result.is_err().then(|| {
        let nodes = fixtures
            .iter()
            .enumerate()
            .map(|(index, (runtime, store, base))| {
                let snapshot = store.load().unwrap().unwrap();
                let worker = if index == 3 {
                    workers.get(4)
                } else {
                    workers.get(index)
                };
                (
                    base,
                    worker.map(|worker| worker.is_finished()),
                    runtime.connected_validator_ids(),
                    snapshot.generation,
                    snapshot
                        .public_checkpoint_proof
                        .as_ref()
                        .map(|proof| proof.checkpoint().epoch()),
                    business_task
                        .as_ref()
                        .and_then(|task| snapshot.state.task_succeeded(task.clone())),
                    PreparedTaskBook::new(store.clone()).map(|book| book.prepared_count()),
                    runtime.drain_bft_consensus_events().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let view = public_store.load().unwrap().unwrap();
        format!(
            "stage={stage}, validators={nodes:?}, public_base={public_base:?}, public_epoch={:?}",
            view.checkpoint_proof
                .as_ref()
                .map(|proof| proof.checkpoint().epoch())
        )
    });
    for worker in workers {
        worker.abort();
        let _ = worker.await;
    }
    client.wait_idle().await;
    if let Some(diagnostic) = failure {
        panic!("operator-to-public-node publication did not converge: {diagnostic}");
    }
    drop(public);
    support::cleanup_public_node_runtime(public_store, public_base);
    for (runtime, store, base) in fixtures {
        drop(runtime);
        support::cleanup_node_runtime(store, base);
    }
    result.expect("operator-to-public-node publication did not converge");
}

#[tokio::test]
async fn restart_resumes_the_signed_public_epoch_without_rebinding_its_digest() {
    let validators = validator_set(1, [1]);
    let base = temp_base("public-checkpoint-restart");
    let store = StateStore::new(&base);
    let state = SecondState::genesis([], 1);
    store.initialize(&state, &validators).unwrap();
    let checkpoint =
        PublicCurrencyCheckpoint::new(CURRENT_PROTOCOL_VERSION, 8, state.public_currency_summary());
    let subject = store
        .public_checkpoint_bft_proposal_subject(&checkpoint)
        .unwrap();
    ValidatorSigner::new(ValidatorId::new(1), key(4), store.clone())
        .sign_bft_prevote(
            subject.scope().clone(),
            0,
            BftValue::Digest(subject.digest()),
            &validators,
            None,
        )
        .unwrap();
    assert_eq!(
        StateStore::new(&base)
            .next_public_currency_checkpoint()
            .unwrap(),
        checkpoint
    );
    let runtime = Arc::new(bind_validator_runtime(
        &store,
        ValidatorRuntimeKeys::new(ValidatorId::new(1), key(3), key(4)),
        support::default_validator_runtime_config(),
    ));
    let worker = support::spawn_node_runtime(&runtime);
    let complete = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(proof) = support::progress::snapshot(&store)
                .await
                .public_checkpoint_proof
            {
                let certified = proof.verify_checkpoint(&validators).unwrap();
                assert_eq!(certified.checkpoint(), &checkpoint);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    worker.abort();
    let _ = worker.await;
    drop(runtime);
    support::cleanup_node_runtime(store, base);
    complete.expect("restart did not complete the signed public checkpoint");
}
