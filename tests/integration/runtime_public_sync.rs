use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use crate::support;
use second::{
    CURRENT_PROTOCOL_VERSION, CertifiedPublicCurrencyCheckpoint, NodeRuntime, Operation,
    PreparedTaskBook, PublicCurrencyCheckpoint, PublicCurrencyCheckpointProof, PublicStateStore,
    SecondState, StateStore, ValidatorId, ValidatorSet, ValidatorVote,
    client_public_currency_checkpoint_proof, client_public_currency_page,
    client_public_currency_summary,
};
use support::{certificate_from_keys, key, signed_vote, validator_set, verified_task};

fn valid_votes(
    checkpoint: &PublicCurrencyCheckpoint,
    validators: &ValidatorSet,
) -> Vec<ValidatorVote> {
    let statement = checkpoint.finality_statement(validators.version());
    (1..=validators.quorum_threshold() as u64)
        .map(|id| signed_vote(&statement, ValidatorId::new(id), &key((id * 3 + 1) as u8)))
        .collect()
}

fn valid_proof(
    state: &SecondState,
    validators: &ValidatorSet,
    epoch: u64,
) -> PublicCurrencyCheckpointProof {
    let checkpoint = PublicCurrencyCheckpoint::new(
        CURRENT_PROTOCOL_VERSION,
        epoch,
        state.public_currency_summary(),
    );
    PublicCurrencyCheckpointProof::new(
        checkpoint.clone(),
        validators.version(),
        valid_votes(&checkpoint, validators),
    )
}

fn invalid_high_epoch_proof(
    state: &SecondState,
    validators: &ValidatorSet,
    epoch: u64,
) -> PublicCurrencyCheckpointProof {
    let checkpoint = PublicCurrencyCheckpoint::new(
        CURRENT_PROTOCOL_VERSION,
        epoch,
        state.public_currency_summary(),
    );
    let statement = checkpoint.finality_statement(validators.version());
    let votes = vec![
        signed_vote(&statement, ValidatorId::new(1), &key(99)),
        signed_vote(&statement, ValidatorId::new(2), &key(7)),
        signed_vote(&statement, ValidatorId::new(3), &key(10)),
    ];
    PublicCurrencyCheckpointProof::new(checkpoint, validators.version(), votes)
}

fn certified_checkpoint(
    state: &SecondState,
    validators: &ValidatorSet,
    epoch: u64,
) -> CertifiedPublicCurrencyCheckpoint {
    let checkpoint = PublicCurrencyCheckpoint::new(
        CURRENT_PROTOCOL_VERSION,
        epoch,
        state.public_currency_summary(),
    );
    let votes = valid_votes(&checkpoint, validators);
    CertifiedPublicCurrencyCheckpoint::new(checkpoint, votes, validators).unwrap()
}

fn runtime_with_proof(
    prefix: &str,
    state: &SecondState,
    validators: &ValidatorSet,
    proof: &PublicCurrencyCheckpointProof,
) -> (Arc<NodeRuntime>, StateStore, std::path::PathBuf) {
    let base = support::temp_base(prefix);
    let store = StateStore::new(&base);
    store.initialize(state, validators).unwrap();
    store.attach_checkpoint_proof(Some(proof)).unwrap();
    let runtime = Arc::new(
        NodeRuntime::load_and_bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), &store).unwrap(),
    );
    (runtime, store, base)
}

#[tokio::test]
async fn runtime_sync_selects_highest_valid_certified_public_checkpoint_without_mutating_private_state()
 {
    let validators = validator_set(1, 1..=4);

    let local_base = support::temp_base("runtime-public-sync-local");
    let local_store = StateStore::new(&local_base);
    let local_state = SecondState::genesis([], 900).with_reserve(2).unwrap();
    local_store.initialize(&local_state, &validators).unwrap();
    local_store
        .advance_checkpoint_floor(&certified_checkpoint(&local_state, &validators, 12))
        .unwrap();
    let local = Arc::new(
        NodeRuntime::load_and_bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), &local_store)
            .unwrap(),
    );

    let stale_state = SecondState::genesis([], 10).with_reserve(2).unwrap();
    let malicious_state = SecondState::genesis([], 20).with_reserve(3).unwrap();
    let fresh_state = SecondState::genesis([], 30).with_reserve(5).unwrap();

    let (stale, stale_store, stale_base) = runtime_with_proof(
        "runtime-public-sync-stale",
        &stale_state,
        &validators,
        &valid_proof(&stale_state, &validators, 10),
    );
    let (malicious, malicious_store, malicious_base) = runtime_with_proof(
        "runtime-public-sync-malicious",
        &malicious_state,
        &validators,
        &invalid_high_epoch_proof(&malicious_state, &validators, 999),
    );
    let (fresh, fresh_store, fresh_base) = runtime_with_proof(
        "runtime-public-sync-fresh",
        &fresh_state,
        &validators,
        &valid_proof(&fresh_state, &validators, 20),
    );

    let stale_task = support::spawn_node_runtime(&stale);
    let malicious_task = support::spawn_node_runtime(&malicious);
    let fresh_task = support::spawn_node_runtime(&fresh);

    let stale_peer = local.dial(&support::peer_record(&stale)).await.unwrap();
    let malicious_peer = local.dial(&support::peer_record(&malicious)).await.unwrap();
    let fresh_peer = local.dial(&support::peer_record(&fresh)).await.unwrap();

    let synced = local
        .sync_freshest_certified_public_currency_view()
        .await
        .unwrap();

    assert_eq!(synced.remote_node_id, fresh.node_id());
    assert_eq!(synced.checkpoint.checkpoint().epoch(), 20);
    assert_eq!(synced.view.summary, fresh_state.public_currency_summary());
    assert_eq!(synced.view.states.len(), 5);

    let persisted_local = local_store.load().unwrap().unwrap();
    assert_eq!(persisted_local.state.next_currency_address(), 902);
    assert_eq!(persisted_local.checkpoint_floor_epoch, 12);
    assert!(persisted_local.public_checkpoint_proof.is_none());

    stale_peer.close();
    malicious_peer.close();
    fresh_peer.close();

    stale_task.abort();
    malicious_task.abort();
    fresh_task.abort();
    let _ = stale_task.await;
    let _ = malicious_task.await;
    let _ = fresh_task.await;

    drop(local);
    drop(stale);
    drop(malicious);
    drop(fresh);

    support::cleanup_node_runtime(local_store, local_base);
    support::cleanup_node_runtime(stale_store, stale_base);
    support::cleanup_node_runtime(malicious_store, malicious_base);
    support::cleanup_node_runtime(fresh_store, fresh_base);
}

#[tokio::test]
async fn runtime_public_sync_uses_durable_validator_set_after_online_transition() {
    let initial = validator_set(1, 1..=4);
    let next = validator_set(2, 1..=5);
    let local_state = SecondState::genesis([], 100).with_reserve(1).unwrap();
    let local_base = support::temp_base("runtime-public-sync-online-transition-local");
    let local_store = StateStore::new(&local_base);
    local_store.initialize(&local_state, &initial).unwrap();
    let local = Arc::new(
        NodeRuntime::load_and_bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), &local_store)
            .unwrap(),
    );

    let certified_transition = support::certified_add_validator_transition(
        &initial,
        2,
        1..=4,
        5,
        1..=3,
        local_state.next_currency_address(),
    );
    let transition = local_store
        .prepare_validator_set_transition(certified_transition.transition().clone())
        .unwrap();
    let certified_transition = support::certify_validator_transition(&initial, transition, 1..=3);
    local_store
        .activate_validator_set_transition(&certified_transition)
        .unwrap();
    assert_eq!(local_store.load().unwrap().unwrap().validator_set, next);

    let remote_state = SecondState::genesis([], 200).with_reserve(4).unwrap();
    let remote_proof = valid_proof(&remote_state, &next, 7);
    let (remote, remote_store, remote_base) = runtime_with_proof(
        "runtime-public-sync-online-transition-remote",
        &remote_state,
        &next,
        &remote_proof,
    );
    let remote_task = support::spawn_node_runtime(&remote);

    let peer = local.dial(&support::peer_record(&remote)).await.unwrap();
    let synced = local
        .sync_freshest_certified_public_currency_view()
        .await
        .unwrap();

    assert_eq!(synced.remote_node_id, remote.node_id());
    assert_eq!(synced.checkpoint.checkpoint().epoch(), 7);
    assert_eq!(synced.view.summary, remote_state.public_currency_summary());

    peer.close();
    remote_task.abort();
    let _ = remote_task.await;
    drop(local);
    drop(remote);
    support::cleanup_node_runtime(local_store, local_base);
    support::cleanup_node_runtime(remote_store, remote_base);
}

#[tokio::test]
async fn runtime_public_connection_reads_current_durable_state_and_checkpoint_without_reconnect() {
    let validators = validator_set(1, 1..=4);
    let account = support::account(1);
    let mut state = SecondState::genesis([account], 1);
    let base = support::temp_base("runtime-public-live-snapshot");
    let store = StateStore::new(&base);
    store.initialize(&state, &validators).unwrap();

    let initial_checkpoint = certified_checkpoint(&state, &validators, 1);
    store
        .attach_certified_checkpoint(&initial_checkpoint)
        .unwrap();

    let runtime = Arc::new(
        NodeRuntime::load_and_bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), &store).unwrap(),
    );
    let runtime_task = support::spawn_node_runtime(&runtime);

    let client = support::quic_client(runtime.transport_certificate_der());
    let peer = client.connect(runtime.local_addr().unwrap()).await.unwrap();

    let initial_summary = client_public_currency_summary(&peer).await.unwrap();
    let initial_proof = client_public_currency_checkpoint_proof(&peer)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(initial_summary.summary, state.public_currency_summary());
    assert_eq!(initial_proof.checkpoint().epoch(), 1);

    let task = verified_task(91, vec![Operation::Issue { account, count: 2 }]);
    let mut prepared = PreparedTaskBook::new(store.clone()).unwrap();
    support::allocate_task(&store, &mut state, &task, 2, &validators).unwrap();
    prepared.prepare(&mut state, &task, 2, &validators).unwrap();
    let statement = prepared
        .prepared_finality_statement(task.task_id())
        .unwrap();
    let certificate = certificate_from_keys(
        statement,
        &validators,
        (1_u64..=3).map(|id| (ValidatorId::new(id), key((id * 3 + 1) as u8))),
    );
    prepared
        .commit(&mut state, task.task_id(), &certificate)
        .unwrap();

    let updated_checkpoint = certified_checkpoint(&state, &validators, 2);
    store
        .attach_certified_checkpoint(&updated_checkpoint)
        .unwrap();

    let updated_summary = client_public_currency_summary(&peer).await.unwrap();
    let updated_page = client_public_currency_page(&peer, second::CurrencyAddress::new(0), 3)
        .await
        .unwrap();
    let updated_proof = client_public_currency_checkpoint_proof(&peer)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated_summary.summary, state.public_currency_summary());
    assert_eq!(updated_page.states, state.public_currency_states());
    assert_ne!(updated_summary.summary, initial_summary.summary);
    assert_eq!(updated_proof.checkpoint().epoch(), 2);
    assert_eq!(
        updated_proof.checkpoint().summary(),
        &state.public_currency_summary()
    );

    peer.close();
    runtime_task.abort();
    let _ = runtime_task.await;
    drop(runtime);
    support::cleanup_node_runtime(store, base);
}

#[tokio::test]
async fn public_only_runtime_incrementally_persists_certified_state_from_full_peer() {
    let validators = validator_set(1, 1..=4);
    let account = support::account(71);
    let mut state = SecondState::genesis([account], 1);

    let server_base = support::temp_base("runtime-public-only-server");
    let server_store = StateStore::new(&server_base);
    server_store.initialize(&state, &validators).unwrap();
    let initial_checkpoint = certified_checkpoint(&state, &validators, 1);
    server_store
        .attach_certified_checkpoint(&initial_checkpoint)
        .unwrap();

    let trusted = server_store.load().unwrap().unwrap();
    let public_base = support::temp_base("runtime-public-only-client");
    let public_store = PublicStateStore::new(&public_base);
    public_store
        .initialize(validators.clone(), trusted.validator_registry)
        .unwrap();
    public_store
        .install_certified_view(
            second::PublicCurrencyView::new(
                state.public_currency_summary(),
                state.public_currency_states(),
            )
            .unwrap(),
            &initial_checkpoint,
        )
        .unwrap();

    let task = verified_task(171, vec![Operation::Issue { account, count: 3 }]);
    let mut prepared = PreparedTaskBook::new(server_store.clone()).unwrap();
    support::allocate_task(&server_store, &mut state, &task, 2, &validators).unwrap();
    prepared.prepare(&mut state, &task, 2, &validators).unwrap();
    let statement = prepared
        .prepared_finality_statement(task.task_id())
        .unwrap();
    let certificate = certificate_from_keys(
        statement,
        &validators,
        (1_u64..=3).map(|id| (ValidatorId::new(id), key((id * 3 + 1) as u8))),
    );
    prepared
        .commit(&mut state, task.task_id(), &certificate)
        .unwrap();
    let updated_checkpoint = certified_checkpoint(&state, &validators, 2);
    server_store
        .attach_certified_checkpoint(&updated_checkpoint)
        .unwrap();

    let server = Arc::new(
        NodeRuntime::load_and_bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), &server_store)
            .unwrap(),
    );
    let public = Arc::new(
        NodeRuntime::load_public_and_bind(
            SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            &public_store,
        )
        .unwrap(),
    );
    let server_task = support::spawn_node_runtime(&server);
    let peer = public.dial(&support::peer_record(&server)).await.unwrap();

    assert!(public.sync_public_state_once().await.unwrap());

    let persisted = public_store.load().unwrap().unwrap();
    assert_eq!(
        persisted
            .checkpoint_proof
            .as_ref()
            .unwrap()
            .checkpoint()
            .epoch(),
        2
    );
    assert_eq!(
        persisted.view.unwrap(),
        second::PublicCurrencyView::new(
            state.public_currency_summary(),
            state.public_currency_states(),
        )
        .unwrap()
    );

    peer.close();
    server_task.abort();
    let _ = server_task.await;
    drop(public);
    drop(server);

    support::cleanup_public_node_runtime(public_store, public_base);
    support::cleanup_node_runtime(server_store, server_base);
}

#[tokio::test]
async fn public_only_runtime_advances_validator_trust_before_syncing_new_set_state() {
    let initial = validator_set(1, 1..=4);
    let server_base = support::temp_base("runtime-public-transition-server");
    let server_store = StateStore::new(&server_base);
    let state = SecondState::genesis([], 400).with_reserve(2).unwrap();
    server_store.initialize(&state, &initial).unwrap();

    let initial_persisted = server_store.load().unwrap().unwrap();
    let public_base = support::temp_base("runtime-public-transition-client");
    let public_store = PublicStateStore::new(&public_base);
    public_store
        .initialize(initial.clone(), initial_persisted.validator_registry)
        .unwrap();

    let transition = support::certified_add_validator_transition(
        &initial,
        2,
        1..=4,
        5,
        1..=3,
        state.next_currency_address(),
    );
    let candidate = server_store
        .prepare_validator_set_transition(transition.transition().clone())
        .unwrap();
    let transition = support::certify_validator_transition(&initial, candidate, 1..=3);
    server_store
        .activate_validator_set_transition(&transition)
        .unwrap();
    let next = server_store.load().unwrap().unwrap().validator_set;
    assert_eq!(next.version(), 2);

    let checkpoint = certified_checkpoint(&state, &next, 8);
    server_store
        .attach_certified_checkpoint(&checkpoint)
        .unwrap();

    let server = Arc::new(
        NodeRuntime::load_and_bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), &server_store)
            .unwrap(),
    );
    let public = Arc::new(
        NodeRuntime::load_public_and_bind(
            SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            &public_store,
        )
        .unwrap(),
    );
    let server_task = support::spawn_node_runtime(&server);
    let peer = public.dial(&support::peer_record(&server)).await.unwrap();

    assert!(public.sync_public_state_once().await.unwrap());

    let persisted = public_store.load().unwrap().unwrap();
    assert_eq!(persisted.validator_set, next);
    assert_eq!(
        persisted
            .checkpoint_proof
            .as_ref()
            .unwrap()
            .checkpoint()
            .epoch(),
        8
    );
    assert_eq!(
        persisted.view.unwrap(),
        second::PublicCurrencyView::new(
            state.public_currency_summary(),
            state.public_currency_states(),
        )
        .unwrap()
    );

    peer.close();
    server_task.abort();
    let _ = server_task.await;
    drop(public);
    drop(server);

    support::cleanup_public_node_runtime(public_store, public_base);
    support::cleanup_node_runtime(server_store, server_base);
}
