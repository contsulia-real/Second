use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use crate::support;
use second::{
    CURRENT_PROTOCOL_VERSION, CertifiedPublicCurrencyCheckpoint, NodeRuntime,
    PublicCurrencyCheckpoint, PublicCurrencyCheckpointProof, SecondState, StateStore, ValidatorId,
    ValidatorSet, ValidatorVote,
};
use support::{key, signed_vote, validator_set};

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

    let certified_transition =
        support::certified_add_validator_transition(&initial, 2, 1..=4, 5, 1..=3);
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
