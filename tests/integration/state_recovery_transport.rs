use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use second::{
    CertifiedStateRecoveryCheckpoint, NetworkError, NodeRuntime, PersistenceError,
    PreparedTaskBook, QuicClient, QuicTransportIdentity, SecondState, StateRecoveryCheckpoint,
    StateStore, ValidatorId, ValidatorSigner, ValidatorSigningError, client_fetch_state_recovery,
};

use crate::support::{self, key, signed_vote, validator_set};

fn certified_checkpoint(store: &StateStore, serial: u64) -> CertifiedStateRecoveryCheckpoint {
    let persisted = store.load().unwrap().unwrap();
    let checkpoint = StateRecoveryCheckpoint::from_persisted(serial, &persisted).unwrap();
    let statement = checkpoint.finality_statement();
    let validators = persisted.validator_set.clone();
    let votes = [1_u64, 2, 3]
        .into_iter()
        .map(|id| signed_vote(&statement, ValidatorId::new(id), &key((id * 3 + 1) as u8)))
        .collect();
    CertifiedStateRecoveryCheckpoint::new(checkpoint, votes, &validators).unwrap()
}

#[tokio::test]
async fn privileged_recovery_chunks_large_private_state_and_installs_only_shared_state() {
    let validators = validator_set(7, 1..=4);
    let accounts = (1..=3000).map(support::account).collect::<Vec<_>>();
    let source_state = SecondState::genesis(accounts, 1).with_reserve(3).unwrap();
    let source_base = support::temp_base("state-recovery-source");
    let source_store = StateStore::new(&source_base);
    source_store.initialize(&source_state, &validators).unwrap();

    let checkpoint = certified_checkpoint(&source_store, 41);
    let source = Arc::new(
        NodeRuntime::load_and_bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), &source_store)
            .unwrap(),
    );
    source
        .publish_state_recovery_checkpoint(checkpoint.clone())
        .unwrap();
    let source_task = support::spawn_node_runtime(&source);

    let client_identity = QuicTransportIdentity::generate().unwrap();
    let client = QuicClient::new(
        SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
        source.transport_certificate_der(),
        client_identity,
    )
    .unwrap();
    let peer = client.connect(source.local_addr().unwrap()).await.unwrap();

    let recovered = client_fetch_state_recovery(&peer, ValidatorId::new(1), &key(3), &validators)
        .await
        .unwrap();

    assert_eq!(recovered.checkpoint.checkpoint().serial(), 41);
    assert_eq!(
        StateRecoveryCheckpoint::from_payload(41, &recovered.payload).unwrap(),
        *checkpoint.checkpoint()
    );
    assert_eq!(recovered.payload.validator_set(), &validators);
    assert_eq!(
        recovered.payload.validator_registry(),
        &source_store.load().unwrap().unwrap().validator_registry
    );
    assert!(recovered.encoded_payload_len > 64 * 1024);

    let destination_base = support::temp_base("state-recovery-destination");
    let destination_store = StateStore::new(&destination_base);
    destination_store
        .install_recovered_state(&recovered.payload, &recovered.checkpoint, &validators)
        .unwrap();

    let installed = destination_store.load().unwrap().unwrap();
    assert_eq!(
        installed.state.current_supply(),
        source_state.current_supply()
    );
    assert_eq!(
        installed.state.reserve_count(),
        source_state.reserve_count()
    );
    assert_eq!(
        installed.state.next_currency_address(),
        source_state.next_currency_address()
    );
    assert_eq!(
        StateRecoveryCheckpoint::from_persisted(41, &installed).unwrap(),
        *checkpoint.checkpoint()
    );
    assert_eq!(installed.validator_set, validators);
    assert_eq!(
        installed.validator_registry,
        source_store.load().unwrap().unwrap().validator_registry
    );
    assert!(!installed.validator_safety_ready);

    let stale_installed_checkpoint = certified_checkpoint(&destination_store, 40);
    assert!(matches!(
        destination_store.advance_recovery_checkpoint_floor(&stale_installed_checkpoint),
        Err(PersistenceError::StaleRecoveryCheckpointSerial {
            validator_set_version: 7,
            minimum: 41,
            actual: 40,
        })
    ));

    assert_eq!(
        PreparedTaskBook::new(destination_store.clone())
            .unwrap()
            .prepared_count(),
        0
    );
    assert_eq!(
        destination_store.install_recovered_state(
            &recovered.payload,
            &recovered.checkpoint,
            &validators,
        ),
        Err(PersistenceError::AlreadyInitialized)
    );

    let signer = ValidatorSigner::new(ValidatorId::new(1), key(4), destination_store.clone());
    let post_recovery =
        StateRecoveryCheckpoint::from_persisted(42, &destination_store.load().unwrap().unwrap())
            .unwrap();
    assert_eq!(
        signer.sign_state_recovery_checkpoint(&post_recovery, &validators),
        Err(ValidatorSigningError::LocalSafetyStateUnavailable)
    );

    peer.close();
    source_task.abort();
    let _ = source_task.await;
    drop(source);
    support::cleanup_node_runtime(source_store, source_base);
    support::cleanup_node_runtime(destination_store, destination_base);
}

#[tokio::test]
async fn recovery_payload_is_denied_without_current_validator_identity_proof() {
    let validators = validator_set(7, 1..=4);
    let state = SecondState::genesis([], 1).with_reserve(1).unwrap();
    let base = support::temp_base("state-recovery-auth");
    let store = StateStore::new(&base);
    store.initialize(&state, &validators).unwrap();

    let runtime = Arc::new(
        NodeRuntime::load_and_bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), &store).unwrap(),
    );
    runtime
        .publish_state_recovery_checkpoint(certified_checkpoint(&store, 5))
        .unwrap();
    let task = support::spawn_node_runtime(&runtime);

    let client = QuicClient::new(
        SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
        runtime.transport_certificate_der(),
        QuicTransportIdentity::generate().unwrap(),
    )
    .unwrap();
    let peer = client.connect(runtime.local_addr().unwrap()).await.unwrap();

    assert!(matches!(
        client_fetch_state_recovery(&peer, ValidatorId::new(1), &key(99), &validators).await,
        Err(NetworkError::StateRecoveryUnauthorized)
    ));
    assert!(matches!(
        client_fetch_state_recovery(&peer, ValidatorId::new(99), &key(3), &validators).await,
        Err(NetworkError::StateRecoveryUnauthorized)
    ));

    peer.close();
    task.abort();
    let _ = task.await;
    drop(runtime);
    support::cleanup_node_runtime(store, base);
}

#[tokio::test]
async fn runtime_persists_recovery_floor_and_refuses_stale_certified_publication_after_restart() {
    let validators = validator_set(7, 1..=4);
    let state = SecondState::genesis([], 1).with_reserve(1).unwrap();
    let base = support::temp_base("state-recovery-publish-floor");
    let store = StateStore::new(&base);
    store.initialize(&state, &validators).unwrap();

    let current = certified_checkpoint(&store, 41);
    {
        let runtime =
            NodeRuntime::load_and_bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), &store).unwrap();
        runtime.publish_state_recovery_checkpoint(current).unwrap();
    }

    let stale = certified_checkpoint(&store, 40);
    let restarted =
        NodeRuntime::load_and_bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), &store).unwrap();
    assert!(matches!(
        restarted.publish_state_recovery_checkpoint(stale),
        Err(second::NodeRuntimeError::Persistence(
            PersistenceError::StaleRecoveryCheckpointSerial {
                validator_set_version: 7,
                minimum: 41,
                actual: 40,
            }
        ))
    ));

    let next = certified_checkpoint(&store, 42);
    restarted.publish_state_recovery_checkpoint(next).unwrap();

    drop(restarted);
    support::cleanup_node_runtime(store, base);
}
