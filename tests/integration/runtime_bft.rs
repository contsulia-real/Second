use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use second::{
    BftConsensusEvent, BftTimeoutConfig, CURRENT_PROTOCOL_VERSION, CertifiedValidatorSetTransition,
    NetworkError, NodeRuntime, NodeRuntimeCapabilities, Operation, PreparationError,
    PreparedTaskBook, PublicCurrencyCheckpoint, QuicClient, QuicTransportIdentity, SecondState,
    StateStore, ValidatorBftRuntimeError, ValidatorConsensusKeyRotationRequest,
    ValidatorCredential, ValidatorId, ValidatorRegistry, ValidatorRotationAuthority,
    ValidatorRuntimeConfig, ValidatorRuntimeKeys, ValidatorSet, ValidatorSetTransition,
    ValidatorSigner, authenticate_validator_bft_peer, client_ping,
};

use crate::support::{self, key, peer_record, single_validator_set, temp_base, validator_set};

fn bind_validator_runtime(
    store: &StateStore,
    keys: ValidatorRuntimeKeys,
    config: ValidatorRuntimeConfig,
) -> NodeRuntime {
    let persisted = store.load().unwrap().unwrap();
    NodeRuntime::bind_loaded(
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        store,
        persisted,
        NodeRuntimeCapabilities::default().with_validator(keys, config),
    )
    .unwrap()
}

fn validator_runtime_fixture(
    prefix: &str,
    validator_id: u64,
) -> (Arc<NodeRuntime>, StateStore, PathBuf) {
    validator_runtime_fixture_with_timeouts(
        prefix,
        validator_id,
        BftTimeoutConfig::new(
            Duration::from_secs(2),
            Duration::from_secs(2),
            Duration::from_secs(2),
        ),
    )
}

fn validator_runtime_fixture_with_timeouts(
    prefix: &str,
    validator_id: u64,
    timeouts: BftTimeoutConfig,
) -> (Arc<NodeRuntime>, StateStore, PathBuf) {
    let base = temp_base(prefix);
    let store = StateStore::new(&base);
    let state = SecondState::genesis([], 1).with_reserve(2).unwrap();
    store.initialize(&state, &validator_set(1, 1..=4)).unwrap();
    let runtime = Arc::new(bind_validator_runtime(
        &store,
        ValidatorRuntimeKeys::new(
            ValidatorId::new(validator_id),
            key((validator_id * 3) as u8),
            key((validator_id * 3 + 1) as u8),
        ),
        support::validator_runtime_config(timeouts),
    ));
    (runtime, store, base)
}

#[tokio::test]
async fn exact_pending_legal_task_retry_is_idempotent() {
    let base = temp_base("runtime-submit-retry");
    let store = StateStore::new(&base);
    let alice = support::account(70);
    store
        .initialize(
            &SecondState::genesis([alice], 1),
            &single_validator_set(1, 1, 31, 32, 33),
        )
        .unwrap();
    let runtime = Arc::new(bind_validator_runtime(
        &store,
        ValidatorRuntimeKeys::new(ValidatorId::new(1), key(31), key(32)),
        support::default_validator_runtime_config(),
    ));
    let task = support::verified_task(
        1400,
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );
    let signed = task.signed_task().clone();

    assert_eq!(
        runtime.submit_legal_task(signed.clone()).unwrap(),
        second::LegalTaskSubmissionOutcome::Prepared
    );
    assert_eq!(
        runtime.submit_legal_task(signed).unwrap(),
        second::LegalTaskSubmissionOutcome::AlreadyPending
    );

    drop(runtime);
    support::cleanup_node_runtime(store, base);
}

fn rotated_consensus_key(validator_id: u64) -> ed25519_dalek::SigningKey {
    key((100 + validator_id) as u8)
}

fn rotated_validator_credential(validator_id: u64) -> ValidatorCredential {
    ValidatorCredential::new(
        ValidatorId::new(validator_id),
        key((validator_id * 3) as u8).verifying_key().to_bytes(),
        rotated_consensus_key(validator_id)
            .verifying_key()
            .to_bytes(),
        key((validator_id * 3 + 2) as u8).verifying_key().to_bytes(),
    )
    .unwrap()
}

fn certified_rotated_validator_set(current: &ValidatorSet) -> CertifiedValidatorSetTransition {
    let next = ValidatorSet::new(5, (1..=4).map(rotated_validator_credential)).unwrap();
    let registry = ValidatorRegistry::from_validator_set(current).unwrap();
    let rotations = (1..=4)
        .map(|validator_id| {
            ValidatorConsensusKeyRotationRequest::sign(
                CURRENT_PROTOCOL_VERSION,
                ValidatorRotationAuthority::Identity,
                ValidatorId::new(validator_id),
                current.version(),
                rotated_consensus_key(validator_id)
                    .verifying_key()
                    .to_bytes(),
                &key((validator_id * 3) as u8),
            )
            .unwrap()
        })
        .collect();
    let transition = ValidatorSetTransition::new(
        CURRENT_PROTOCOL_VERSION,
        current,
        &registry,
        next,
        Vec::new(),
        rotations,
    )
    .unwrap();
    let statement = transition.finality_statement();
    let votes = [1_u64, 2, 3]
        .into_iter()
        .map(|validator_id| {
            support::signed_vote(
                &statement,
                ValidatorId::new(validator_id),
                &key((validator_id * 3 + 1) as u8),
            )
        })
        .collect();
    CertifiedValidatorSetTransition::new(transition, votes, current).unwrap()
}

fn public_checkpoint(store: &StateStore, epoch: u64) -> PublicCurrencyCheckpoint {
    let persisted = store.load().unwrap().unwrap();
    PublicCurrencyCheckpoint::new(
        CURRENT_PROTOCOL_VERSION,
        epoch,
        persisted.state.public_currency_summary(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
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
            .start_public_checkpoint_consensus(checkpoint)
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_validator_source_bootstraps_prepared_task_consensus_by_private_pull() {
    let validators = validator_set(1, 1..=4);
    let alice = support::account(31);
    let task = support::verified_task(
        1300,
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );
    let task_id = task.task_id();

    let mut fixtures = (1..=4)
        .map(|validator_id| {
            let base = temp_base(&format!(
                "runtime-bft-private-task-pull-validator-{validator_id}"
            ));
            let store = StateStore::new(&base);
            store
                .initialize(&SecondState::genesis([alice], 1), &validators)
                .unwrap();
            let runtime = Arc::new(bind_validator_runtime(
                &store,
                ValidatorRuntimeKeys::new(
                    ValidatorId::new(validator_id),
                    key((validator_id * 3) as u8),
                    key((validator_id * 3 + 1) as u8),
                ),
                support::default_validator_runtime_config(),
            ));
            (runtime, store, base)
        })
        .collect::<Vec<_>>();

    for (_, store, _) in &fixtures {
        assert!(
            !PreparedTaskBook::new(store.clone())
                .unwrap()
                .is_prepared(task_id.clone())
        );
    }

    let records = fixtures
        .iter()
        .map(|(runtime, _, _)| peer_record(runtime))
        .collect::<Vec<_>>();
    let runtime_tasks = fixtures
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

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if fixtures
                .iter()
                .all(|(runtime, _, _)| runtime.connected_validator_ids().len() == 3)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("all validators must establish private BFT connections before task submission");

    // Validator 1 is the deterministic round-0 proposer. Submit only to Validator 2 so
    // consensus can start only if the private availability hint and on-demand pull work.
    assert_eq!(
        fixtures[1]
            .0
            .submit_legal_task(task.signed_task().clone())
            .unwrap(),
        second::LegalTaskSubmissionOutcome::Prepared
    );

    let mut certificates = vec![None; fixtures.len()];
    let mut send_failures = vec![Vec::new(); fixtures.len()];
    let completed = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            for (index, (runtime, _, _)) in fixtures.iter().enumerate() {
                for event in runtime.drain_bft_consensus_events().unwrap() {
                    match event {
                        BftConsensusEvent::CertifiedPreparedTask {
                            task_id: certified_task_id,
                            certificate,
                        } => {
                            assert_eq!(certified_task_id, task_id);
                            certificate.verify(&validators).unwrap();
                            certificates[index] = Some(certificate);
                        }
                        event @ BftConsensusEvent::SendFailed { .. } => {
                            send_failures[index].push(event);
                        }
                        event => panic!("unexpected task propagation consensus event: {event:?}"),
                    }
                }
            }
            if certificates.iter().all(Option::is_some) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    if completed.is_err() {
        let states = fixtures
            .iter()
            .enumerate()
            .map(|(index, (runtime, store, _))| {
                let validator_id = ValidatorId::new((index + 1) as u64);
                let prepared = PreparedTaskBook::new(store.clone())
                    .unwrap()
                    .is_prepared(task_id.clone());
                let subject = store.prepared_bft_proposal_subject(task_id.clone());
                let bft_state = subject.as_ref().ok().and_then(|subject| {
                    store
                        .bft_local_state(validator_id, subject.scope())
                        .unwrap()
                });
                (
                    validator_id,
                    runtime.connected_validator_ids(),
                    prepared,
                    subject,
                    bft_state,
                    runtime.drain_bft_consensus_events().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let task_finished = runtime_tasks
            .iter()
            .map(|task| task.is_finished())
            .collect::<Vec<_>>();
        panic!(
            "private task pull stalled; certificates={certificates:?}; send_failures={send_failures:?}; states={states:?}; task_finished={task_finished:?}"
        );
    }

    for (_, store, _) in &fixtures {
        let book = PreparedTaskBook::new(store.clone()).unwrap();
        assert!(!book.is_prepared(task_id.clone()));
        let committed = store.load().unwrap().unwrap();
        assert_eq!(committed.state.current_supply(), 1);
    }

    for runtime_task in &runtime_tasks {
        runtime_task.abort();
    }
    for runtime_task in runtime_tasks {
        let _ = runtime_task.await;
    }
    for (runtime, store, base) in fixtures.drain(..) {
        drop(runtime);
        support::cleanup_node_runtime(store, base);
    }
}

#[tokio::test]
async fn runtime_bft_timeout_scheduler_advances_round_without_manual_driver_calls() {
    let timeout_config = BftTimeoutConfig::new(
        Duration::from_millis(25),
        Duration::from_millis(25),
        Duration::from_millis(25),
    );
    let (runtime, store, base) =
        validator_runtime_fixture_with_timeouts("runtime-bft-timeout", 2, timeout_config);
    let task = support::spawn_node_runtime(&runtime);
    let checkpoint = public_checkpoint(&store, 1);
    let scope = store
        .public_checkpoint_bft_proposal_subject(&checkpoint)
        .unwrap()
        .scope()
        .clone();

    runtime
        .start_public_checkpoint_consensus(checkpoint)
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn running_bft_sessions_refresh_active_set_and_keep_retained_prepared_authority() {
    let current = validator_set(4, 1..=4);
    let certified_transition = certified_rotated_validator_set(&current);
    let alice = support::account(1);
    let task = support::verified_task(
        1200,
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );

    let mut fixtures = (1..=4)
        .map(|validator_id| {
            let base = temp_base(&format!(
                "runtime-bft-retained-prepared-validator-{validator_id}"
            ));
            let store = StateStore::new(&base);
            let mut state = SecondState::genesis([alice], 1);
            {
                let mut book = PreparedTaskBook::new(store.clone()).unwrap();
                book.prepare(&mut state, &task, 1, &current).unwrap();
            }
            let keys = ValidatorRuntimeKeys::new(
                ValidatorId::new(validator_id),
                key((validator_id * 3) as u8),
                key((validator_id * 3 + 1) as u8),
            )
            .with_consensus_key(rotated_consensus_key(validator_id));
            let runtime = Arc::new(bind_validator_runtime(
                &store,
                keys,
                support::default_validator_runtime_config(),
            ));
            (runtime, store, base)
        })
        .collect::<Vec<_>>();

    let records = fixtures
        .iter()
        .map(|(runtime, _, _)| peer_record(runtime))
        .collect::<Vec<_>>();
    let runtime_tasks = fixtures
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

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if fixtures
                .iter()
                .all(|(runtime, _, _)| runtime.connected_validator_ids().len() == 3)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("retained-set validators must establish BFT sessions through stable identity keys");

    for (_, store, _) in &fixtures {
        store
            .activate_validator_set_transition(&certified_transition)
            .unwrap();
        let persisted = store.load().unwrap().unwrap();
        assert_eq!(persisted.validator_set.version(), 5);
        assert_eq!(persisted.retained_validator_sets.get(&4), Some(&current));
    }

    let timeouts = BftTimeoutConfig::new(
        Duration::from_secs(2),
        Duration::from_secs(2),
        Duration::from_secs(2),
    );
    let active_v5 = fixtures[0].1.load().unwrap().unwrap().validator_set;
    let checkpoints = fixtures
        .iter()
        .map(|(_, store, _)| public_checkpoint(store, 1))
        .collect::<Vec<_>>();
    assert!(
        checkpoints
            .iter()
            .all(|checkpoint| checkpoint == &checkpoints[0])
    );
    for ((runtime, _, _), checkpoint) in fixtures.iter().zip(checkpoints.iter().cloned()) {
        runtime
            .start_public_checkpoint_consensus(checkpoint)
            .unwrap();
    }

    let consensus_timeout =
        timeouts.proposal + timeouts.prevote + timeouts.precommit + timeouts.precommit;
    let convergence_timeout = consensus_timeout + consensus_timeout;
    let mut active_certified = [false; 4];
    tokio::time::timeout(convergence_timeout, async {
        loop {
            for (index, (runtime, _, _)) in fixtures.iter().enumerate() {
                for event in runtime.drain_bft_consensus_events().unwrap() {
                    match event {
                        BftConsensusEvent::CertifiedPublicCheckpoint(value) => {
                            assert_eq!(value.checkpoint(), &checkpoints[0]);
                            value.certificate().verify(&active_v5).unwrap();
                            assert_eq!(value.certificate().statement().validator_set_version(), 5);
                            active_certified[index] = true;
                        }
                        event => panic!("unexpected consensus event: {event:?}"),
                    }
                }
            }
            if active_certified.iter().all(|ready| *ready) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("running BFT sessions must refresh to the newly activated ValidatorSet");

    for (runtime, _, _) in &fixtures {
        runtime
            .start_prepared_task_consensus(task.task_id())
            .unwrap();
    }

    let mut certificates = vec![None; fixtures.len()];
    let retained_timeout = convergence_timeout;
    let prepared_scope = fixtures[0]
        .1
        .prepared_bft_proposal_subject(task.task_id())
        .unwrap()
        .scope()
        .clone();
    let retained_result = tokio::time::timeout(retained_timeout, async {
        loop {
            for (index, (runtime, _, _)) in fixtures.iter().enumerate() {
                for event in runtime.drain_bft_consensus_events().unwrap() {
                    match event {
                        BftConsensusEvent::CertifiedPreparedTask {
                            task_id,
                            certificate,
                        } => {
                            assert_eq!(task_id, task.task_id());
                            assert_eq!(certificate.statement().validator_set_version(), 4);
                            certificate.verify(&current).unwrap();
                            certificates[index] = Some(certificate);
                        }
                        event => panic!("unexpected consensus event: {event:?}"),
                    }
                }
            }
            if certificates.iter().all(Option::is_some) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    if retained_result.is_err() {
        let states = fixtures
            .iter()
            .enumerate()
            .map(|(index, (runtime, store, _))| {
                (
                    index + 1,
                    runtime.connected_validator_ids(),
                    store
                        .bft_local_state(ValidatorId::new((index + 1) as u64), &prepared_scope)
                        .unwrap(),
                    ValidatorSigner::new(
                        ValidatorId::new((index + 1) as u64),
                        key(((index + 1) as u64 * 3 + 1) as u8),
                        store.clone(),
                    )
                    .prepared_task_lock(task.task_id())
                    .unwrap(),
                    runtime_tasks[index].is_finished(),
                    runtime.drain_bft_consensus_events().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        panic!(
            "retained PreparedTask must finalize with its exact historical ValidatorSet; certified={certificates:?}; states={states:?}"
        );
    }

    for (_, store, _) in &fixtures {
        let mut book = PreparedTaskBook::new(store.clone()).unwrap();
        assert_eq!(
            book.cancel(task.task_id()),
            Err(PreparationError::NotPrepared(task.task_id()))
        );
        let committed = store.load().unwrap().unwrap();
        assert!(committed.retained_validator_sets.is_empty());
        assert_eq!(committed.state.current_supply(), 1);
    }

    for runtime_task in &runtime_tasks {
        runtime_task.abort();
    }
    for runtime_task in runtime_tasks {
        let _ = runtime_task.await;
    }
    for (runtime, store, base) in fixtures.drain(..) {
        drop(runtime);
        support::cleanup_node_runtime(store, base);
    }
}

#[tokio::test]
async fn retained_prepared_task_refuses_current_consensus_key_without_historical_key() {
    let current = validator_set(4, 1..=4);
    let certified_transition = certified_rotated_validator_set(&current);
    let alice = support::account(2);
    let task = support::verified_task(
        1201,
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );
    let base = temp_base("runtime-bft-retained-prepared-missing-key");
    let store = StateStore::new(&base);
    let mut state = SecondState::genesis([alice], 1);
    {
        let mut book = PreparedTaskBook::new(store.clone()).unwrap();
        book.prepare(&mut state, &task, 1, &current).unwrap();
    }
    store
        .activate_validator_set_transition(&certified_transition)
        .unwrap();

    let runtime = bind_validator_runtime(
        &store,
        ValidatorRuntimeKeys::new(ValidatorId::new(1), key(3), rotated_consensus_key(1)),
        support::default_validator_runtime_config(),
    );

    assert!(matches!(
        runtime.start_prepared_task_consensus(task.task_id()),
        Err(second::NodeRuntimeError::ValidatorBft(
            ValidatorBftRuntimeError::ConsensusKeyMismatch(validator_id)
        )) if validator_id == ValidatorId::new(1)
    ));

    drop(runtime);
    support::cleanup_node_runtime(store, base);
}
