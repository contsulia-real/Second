use std::io::Cursor;
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

mod support;

use second::{
    CURRENT_PROTOCOL_VERSION, FinalityError, NetworkError, NetworkMessage, NodeId,
    PublicCheckpointError, PublicCurrencyCheckpoint, PublicCurrencyCheckpointProof, SecondState,
    ValidatorId, ValidatorSet, ValidatorVote, client_sync_certified_public_currency_view,
    read_network_message, serve_public_currency_connection_with_checkpoint, write_network_message,
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

    let mut bytes = Vec::new();
    write_network_message(&mut bytes, &message).unwrap();
    let decoded = read_network_message(&mut Cursor::new(bytes)).unwrap();

    assert_eq!(decoded, message);
    assert_eq!(
        match decoded {
            NetworkMessage::PublicCurrencyCheckpointProof { proof } => proof,
            other => panic!("unexpected message: {other:?}"),
        },
        proof
    );
}

#[test]
fn real_tcp_sync_returns_certified_view_only_after_local_quorum_verification() {
    let validators = validators();
    let state = SecondState::genesis([], 10).with_reserve(300).unwrap();
    let proof = checkpoint_proof(&state, &validators, 12);

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();

        serve_public_currency_connection_with_checkpoint(
            &mut stream,
            NodeId::from_u64(1),
            &state,
            Some(&proof),
        )
        .unwrap()
    });

    let mut client = TcpStream::connect(address).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    client
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();

    let synced = client_sync_certified_public_currency_view(
        &mut client,
        NodeId::from_u64(2),
        &validators,
        0,
    )
    .unwrap();

    assert_eq!(synced.remote_node_id, NodeId::from_u64(1));
    assert_eq!(synced.view.states.len(), 300);
    assert_eq!(synced.checkpoint.checkpoint().epoch(), 12);
    assert_eq!(synced.checkpoint.certificate().vote_count(), 3);
    assert_eq!(
        synced.checkpoint.verify_view(&synced.view, &validators),
        Ok(())
    );

    drop(client);
    assert_eq!(server.join().unwrap(), NodeId::from_u64(2));
}

#[test]
fn stale_but_valid_checkpoint_is_rejected_before_state_sync() {
    let validators = validators();
    let state = SecondState::genesis([], 1).with_reserve(2).unwrap();
    let proof = checkpoint_proof(&state, &validators, 7);

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        serve_public_currency_connection_with_checkpoint(
            &mut stream,
            NodeId::from_u64(1),
            &state,
            Some(&proof),
        )
        .unwrap()
    });

    let mut client = TcpStream::connect(address).unwrap();

    assert_eq!(
        client_sync_certified_public_currency_view(
            &mut client,
            NodeId::from_u64(2),
            &validators,
            8,
        ),
        Err(NetworkError::StalePublicCurrencyCheckpoint {
            minimum_epoch: 8,
            actual_epoch: 7,
        })
    );

    drop(client);
    let _ = server.join().unwrap();
}

#[test]
fn fake_checkpoint_signature_is_rejected_after_successful_state_sync() {
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

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        serve_public_currency_connection_with_checkpoint(
            &mut stream,
            NodeId::from_u64(1),
            &state,
            Some(&proof),
        )
        .unwrap()
    });

    let mut client = TcpStream::connect(address).unwrap();

    assert_eq!(
        client_sync_certified_public_currency_view(
            &mut client,
            NodeId::from_u64(2),
            &validators,
            0
        ),
        Err(NetworkError::PublicCheckpoint(
            PublicCheckpointError::Finality(FinalityError::InvalidSignature(ValidatorId::new(1)))
        ))
    );

    drop(client);
    assert_eq!(server.join().unwrap(), NodeId::from_u64(2));
}

#[test]
fn server_refuses_to_attach_checkpoint_for_a_different_public_state() {
    let validators = validators();
    let served_state = SecondState::genesis([], 1).with_reserve(2).unwrap();
    let other_state = SecondState::genesis([], 10).with_reserve(2).unwrap();
    let proof = checkpoint_proof(&other_state, &validators, 1);
    let mut empty_stream = Cursor::new(Vec::<u8>::new());

    assert_eq!(
        serve_public_currency_connection_with_checkpoint(
            &mut empty_stream,
            NodeId::from_u64(1),
            &served_state,
            Some(&proof),
        ),
        Err(NetworkError::CheckpointDoesNotMatchServedState)
    );
}
