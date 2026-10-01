use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use second::{
    BftConsensusEvent, BftTimeoutConfig, CURRENT_PROTOCOL_VERSION, NetworkError, NodeRuntime,
    PublicCurrencyCheckpoint, QuicClient, QuicTransportIdentity, SecondState, StateStore,
    ValidatorId, ValidatorRuntimeKeys, authenticate_validator_bft_peer, client_ping,
};

use crate::support::{self, key, peer_record, temp_base, validator_set};

fn validator_runtime_fixture(
    prefix: &str,
    validator_id: u64,
) -> (Arc<NodeRuntime>, StateStore, PathBuf) {
    let base = temp_base(prefix);
    let store = StateStore::new(&base);
    let state = SecondState::genesis([], 1).with_reserve(2).unwrap();
    store.initialize(&state, &validator_set(1, 1..=4)).unwrap();
    let runtime = Arc::new(
        NodeRuntime::load_validator_and_bind(
            SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            &store,
            ValidatorRuntimeKeys::new(
                ValidatorId::new(validator_id),
                key((validator_id * 3) as u8),
                key((validator_id * 3 + 1) as u8),
            ),
        )
        .unwrap(),
    );
    (runtime, store, base)
}

fn public_checkpoint(store: &StateStore, epoch: u64) -> PublicCurrencyCheckpoint {
    let persisted = store.load().unwrap().unwrap();
    PublicCurrencyCheckpoint::new(
        CURRENT_PROTOCOL_VERSION,
        epoch,
        persisted.state.public_currency_summary(),
    )
}

#[tokio::test]
async fn four_validator_runtimes_drive_consensus_to_certified_public_checkpoint() {
    let mut fixtures = (1..=4)
        .map(|validator_id| {
            validator_runtime_fixture(
                &format!("runtime-bft-validator-{validator_id}"),
                validator_id,
            )
        })
        .collect::<Vec<_>>();
    let records = fixtures
        .iter()
        .map(|(runtime, _, _)| peer_record(runtime))
        .collect::<Vec<_>>();

    let tasks = fixtures
        .iter()
        .enumerate()
        .map(|(index, (runtime, _, _))| {
            let bootstrap = records
                .iter()
                .enumerate()
                .filter(|(candidate, _)| *candidate != index)
                .map(|(_, record)| record.clone())
                .collect();
            support::spawn_node_runtime_with_bootstrap(runtime, bootstrap)
        })
        .collect::<Vec<_>>();

    let connection_result = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let ready = fixtures.iter().enumerate().all(|(index, (runtime, _, _))| {
                runtime.connected_validator_ids().len() == 3
                    && records.iter().enumerate().all(|(candidate, record)| {
                        candidate == index || runtime.peer(record.node_id()).is_some()
                    })
            });
            if ready {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    if connection_result.is_err() {
        let connections = fixtures
            .iter()
            .enumerate()
            .map(|(index, (runtime, _, _))| {
                let public = records
                    .iter()
                    .enumerate()
                    .filter_map(|(candidate, record)| {
                        (candidate != index && runtime.peer(record.node_id()).is_some())
                            .then_some(candidate + 1)
                    })
                    .collect::<Vec<_>>();
                (index + 1, runtime.connected_validator_ids(), public)
            })
            .collect::<Vec<_>>();
        panic!(
            "all validator runtimes must establish dedicated BFT and public peer connections; connections={connections:?}"
        );
    }

    let checkpoints = fixtures
        .iter()
        .map(|(_, store, _)| public_checkpoint(store, 1))
        .collect::<Vec<_>>();
    assert!(
        checkpoints
            .iter()
            .all(|checkpoint| checkpoint == &checkpoints[0]),
        "all validators must build the same public checkpoint before consensus"
    );
    let expected_checkpoint = checkpoints[0].clone();
    let expected_subject = fixtures[0]
        .1
        .public_checkpoint_bft_proposal_subject(&expected_checkpoint)
        .unwrap();
    let expected_scope = expected_subject.scope().clone();

    let timeouts = BftTimeoutConfig::new(
        Duration::from_secs(2),
        Duration::from_secs(2),
        Duration::from_secs(2),
    );
    let consensus_timeout =
        timeouts.proposal + timeouts.prevote + timeouts.precommit + timeouts.precommit;
    for ((runtime, _, _), checkpoint) in fixtures.iter().zip(checkpoints) {
        runtime
            .start_public_checkpoint_consensus(checkpoint, timeouts)
            .unwrap();
    }

    let mut certified = [false; 4];
    let consensus_result = tokio::time::timeout(consensus_timeout, async {
        loop {
            for (index, (runtime, _, _)) in fixtures.iter().enumerate() {
                for event in runtime.drain_bft_consensus_events().unwrap() {
                    match event {
                        BftConsensusEvent::CertifiedPublicCheckpoint(value) => {
                            assert_eq!(value.checkpoint(), &expected_checkpoint);
                            value
                                .certificate()
                                .verify(&validator_set(1, 1..=4))
                                .unwrap();
                            certified[index] = true;
                        }
                        event => panic!("unexpected consensus event: {event:?}"),
                    }
                }
            }
            if certified.iter().all(|ready| *ready) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    if consensus_result.is_err() {
        let states = fixtures
            .iter()
            .enumerate()
            .map(|(index, (runtime, store, _))| {
                (
                    index + 1,
                    runtime.connected_validator_ids(),
                    store
                        .bft_local_state(ValidatorId::new((index + 1) as u64), &expected_scope)
                        .unwrap(),
                    runtime.drain_bft_consensus_events().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let task_finished = tasks
            .iter()
            .map(|task| task.is_finished())
            .collect::<Vec<_>>();
        panic!(
            "all validators must produce a certified public checkpoint; certified={certified:?}; states={states:?}; task_finished={task_finished:?}"
        );
    }

    let first = &fixtures[0].0;
    let second = &fixtures[1].0;
    let public_peer = first
        .peer(second.node_id())
        .expect("public peer connection must coexist with dedicated BFT connections");
    assert_eq!(
        client_ping(&public_peer, 77).await.unwrap(),
        second.node_id()
    );

    for task in &tasks {
        task.abort();
    }
    for task in tasks {
        let _ = task.await;
    }
    for (runtime, store, base) in fixtures.drain(..) {
        drop(runtime);
        support::cleanup_node_runtime(store, base);
    }
}

#[tokio::test]
async fn runtime_bft_timeout_scheduler_advances_round_without_manual_driver_calls() {
    let (runtime, store, base) = validator_runtime_fixture("runtime-bft-timeout", 2);
    let task = support::spawn_node_runtime(&runtime);
    let checkpoint = public_checkpoint(&store, 1);
    let scope = store
        .public_checkpoint_bft_proposal_subject(&checkpoint)
        .unwrap()
        .scope()
        .clone();

    runtime
        .start_public_checkpoint_consensus(
            checkpoint,
            BftTimeoutConfig::new(
                Duration::from_millis(25),
                Duration::from_millis(25),
                Duration::from_millis(25),
            ),
        )
        .unwrap();

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if store
                .bft_local_state(ValidatorId::new(2), &scope)
                .unwrap()
                .is_some_and(|state| state.round() >= 1)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("runtime timeout scheduler must durably advance the BFT round");

    for event in runtime.drain_bft_consensus_events().unwrap() {
        assert!(
            !matches!(
                event,
                BftConsensusEvent::Rejected { .. } | BftConsensusEvent::UnregisteredScope { .. }
            ),
            "local timeout progression must not reject its own consensus state: {event:?}"
        );
    }

    task.abort();
    let _ = task.await;
    drop(runtime);
    support::cleanup_node_runtime(store, base);
}

#[tokio::test]
async fn public_only_runtime_denies_validator_bft_without_breaking_public_session() {
    let validators = validator_set(1, 1..=4);
    let (public_runtime, public_store, public_base) =
        support::node_runtime_fixture("runtime-bft-public-only");
    let public_record = peer_record(&public_runtime);
    let public_task = support::spawn_node_runtime(&public_runtime);

    let bft_client = QuicClient::new(
        SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
        public_record.certificate_der(),
        QuicTransportIdentity::generate().unwrap(),
    )
    .unwrap();
    let bft_peer = bft_client
        .connect_expected(public_record.address(), public_record.node_id())
        .await
        .unwrap();
    assert!(matches!(
        authenticate_validator_bft_peer(bft_peer, ValidatorId::new(1), &key(3), &validators,).await,
        Err(NetworkError::BftUnauthorized)
    ));

    let (caller, caller_store, caller_base) =
        support::node_runtime_fixture("runtime-bft-public-caller");
    let public_peer = caller.dial(&public_record).await.unwrap();
    assert_eq!(
        client_ping(&public_peer, 88).await.unwrap(),
        public_runtime.node_id()
    );

    public_peer.close();
    public_task.abort();
    let _ = public_task.await;
    drop(caller);
    drop(public_runtime);
    support::cleanup_node_runtime(caller_store, caller_base);
    support::cleanup_node_runtime(public_store, public_base);
}
