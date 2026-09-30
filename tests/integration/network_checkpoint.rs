use crate::support;

use second::{
    CURRENT_PROTOCOL_VERSION, FinalityError, NetworkError, NetworkMessage, PublicCheckpointError,
    PublicCurrencyCheckpoint, PublicCurrencyCheckpointProof, SecondState, ValidatorId,
    ValidatorSet, ValidatorVote, client_sync_certified_public_currency_view,
    decode_network_message, encode_network_message, serve_public_currency_connection,
};
use support::{key, signed_vote, validator_set};

fn validators() -> ValidatorSet {
    validator_set(4, 1..=4)
}

fn checkpoint_proof(
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
    let votes = [1_u64, 2, 3]
        .into_iter()
        .map(|id| signed_vote(&statement, ValidatorId::new(id), &key((id * 3 + 1) as u8)))
        .collect();

    PublicCurrencyCheckpointProof::new(checkpoint, validators.version(), votes)
}

#[test]
fn checkpoint_proof_round_trips_as_unverified_network_data() {
    let state = SecondState::genesis([], 10).with_reserve(3).unwrap();
    let proof = PublicCurrencyCheckpointProof::new(
        PublicCurrencyCheckpoint::new(CURRENT_PROTOCOL_VERSION, 7, state.public_currency_summary()),
        4,
        vec![
            ValidatorVote::from_untrusted_parts(ValidatorId::new(1), [11; 64]),
            ValidatorVote::from_untrusted_parts(ValidatorId::new(2), [22; 64]),
        ],
    );
    let message = NetworkMessage::PublicCurrencyCheckpointProof {
        proof: proof.clone(),
    };

    let bytes = encode_network_message(&message).unwrap();
    let decoded = decode_network_message(&bytes).unwrap();

    assert_eq!(decoded, message);
    assert_eq!(
        match decoded {
            NetworkMessage::PublicCurrencyCheckpointProof { proof } => proof,
            other => panic!("unexpected message: {other:?}"),
        },
        proof
    );
}

#[tokio::test]
async fn real_quic_sync_returns_certified_view_only_after_local_quorum_verification() {
    let validators = validators();
    let state = SecondState::genesis([], 10).with_reserve(300).unwrap();
    let proof = checkpoint_proof(&state, &validators, 12);
    let (server, certificate) = support::quic_server();
    let address = server.local_addr().unwrap();
    let server_node_id = server.node_id();

    let server_task = tokio::spawn(async move {
        let peer = server.accept().await.unwrap();
        serve_public_currency_connection(&peer, &state, Some(&proof))
            .await
            .unwrap()
    });

    let client = support::quic_client(&certificate);
    let client_node_id = client.node_id();
    let peer = client.connect(address).await.unwrap();

    let synced = client_sync_certified_public_currency_view(&peer, &validators, 0)
        .await
        .unwrap();

    assert_eq!(synced.remote_node_id, server_node_id);
    assert_eq!(synced.view.states.len(), 300);
    assert_eq!(synced.checkpoint.checkpoint().epoch(), 12);
    assert_eq!(synced.checkpoint.certificate().vote_count(), 3);
    assert_eq!(
        synced.checkpoint.verify_view(&synced.view, &validators),
        Ok(())
    );

    peer.close();
    assert_eq!(server_task.await.unwrap(), client_node_id);
}

#[tokio::test]
async fn stale_but_valid_checkpoint_is_rejected_before_state_sync() {
    let validators = validators();
    let state = SecondState::genesis([], 1).with_reserve(2).unwrap();
    let proof = checkpoint_proof(&state, &validators, 7);
    let (server, certificate) = support::quic_server();
    let address = server.local_addr().unwrap();

    let server_task = tokio::spawn(async move {
        let peer = server.accept().await.unwrap();
        serve_public_currency_connection(&peer, &state, Some(&proof))
            .await
            .unwrap()
    });

    let client = support::quic_client(&certificate);
    let peer = client.connect(address).await.unwrap();

    assert_eq!(
        client_sync_certified_public_currency_view(&peer, &validators, 8).await,
        Err(NetworkError::StalePublicCurrencyCheckpoint {
            minimum_epoch: 8,
            actual_epoch: 7,
        })
    );

    peer.close();
    let _ = server_task.await.unwrap();
}

#[tokio::test]
async fn fake_checkpoint_signature_is_rejected_after_successful_state_sync() {
    let validators = validators();
    let state = SecondState::genesis([], 1).with_reserve(2).unwrap();
    let checkpoint =
        PublicCurrencyCheckpoint::new(CURRENT_PROTOCOL_VERSION, 3, state.public_currency_summary());
    let statement = checkpoint.finality_statement(validators.version());
    let proof = PublicCurrencyCheckpointProof::new(
        checkpoint,
        validators.version(),
        vec![
            signed_vote(&statement, ValidatorId::new(1), &key(99)),
            signed_vote(&statement, ValidatorId::new(2), &key(7)),
            signed_vote(&statement, ValidatorId::new(3), &key(10)),
        ],
    );

    let (server, certificate) = support::quic_server();
    let address = server.local_addr().unwrap();

    let server_task = tokio::spawn(async move {
        let peer = server.accept().await.unwrap();
        serve_public_currency_connection(&peer, &state, Some(&proof))
            .await
            .unwrap()
    });

    let client = support::quic_client(&certificate);
    let client_node_id = client.node_id();
    let peer = client.connect(address).await.unwrap();

    assert_eq!(
        client_sync_certified_public_currency_view(&peer, &validators, 0).await,
        Err(NetworkError::PublicCheckpoint(
            PublicCheckpointError::Finality(FinalityError::InvalidSignature(ValidatorId::new(1)))
        ))
    );

    peer.close();
    assert_eq!(server_task.await.unwrap(), client_node_id);
}

#[tokio::test]
async fn server_refuses_to_attach_checkpoint_for_a_different_public_state() {
    let validators = validators();
    let served_state = SecondState::genesis([], 1).with_reserve(2).unwrap();
    let other_state = SecondState::genesis([], 10).with_reserve(2).unwrap();
    let proof = checkpoint_proof(&other_state, &validators, 1);

    let (server, certificate) = support::quic_server();
    let address = server.local_addr().unwrap();

    let server_task = tokio::spawn(async move {
        let peer = server.accept().await.unwrap();
        serve_public_currency_connection(&peer, &served_state, Some(&proof)).await
    });

    let client = support::quic_client(&certificate);
    let peer = client.connect(address).await.unwrap();

    assert_eq!(
        server_task.await.unwrap(),
        Err(NetworkError::CheckpointDoesNotMatchServedState)
    );
    peer.close();
}
