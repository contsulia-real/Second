use std::io::Cursor;
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use second::{
    CURRENT_PROTOCOL_VERSION, CertifiedPublicCurrencyCheckpoint, FinalityError, NetworkError,
    NetworkMessage, NodeId, PublicCheckpointError, PublicCurrencyCheckpoint,
    PublicCurrencyCheckpointProof, SecondState, ValidatorCredential, ValidatorId, ValidatorSet,
    ValidatorVote, client_sync_certified_public_currency_view, read_network_message,
    serve_public_currency_connection_with_checkpoint, write_network_message,
};

fn key(byte: u8) -> SigningKey {
    SigningKey::from_bytes(&[byte; 32])
}

fn credential(id: u64) -> ValidatorCredential {
    ValidatorCredential::new(
        ValidatorId::new(id),
        key((id * 3) as u8).verifying_key().to_bytes(),
        key((id * 3 + 1) as u8).verifying_key().to_bytes(),
        key((id * 3 + 2) as u8).verifying_key().to_bytes(),
    )
    .unwrap()
}

fn validators() -> ValidatorSet {
    ValidatorSet::new(4, (1..=4).map(credential)).unwrap()
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
        .map(|id| ValidatorVote::sign(&statement, ValidatorId::new(id), &key((id * 3 + 1) as u8)))
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
            ValidatorVote::from_parts(ValidatorId::new(1), [11; 64]),
            ValidatorVote::from_parts(ValidatorId::new(2), [22; 64]),
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

    let synced =
        client_sync_certified_public_currency_view(&mut client, NodeId::from_u64(2), &validators)
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
            ValidatorVote::sign(&statement, ValidatorId::new(1), &key(99)),
            ValidatorVote::sign(&statement, ValidatorId::new(2), &key(7)),
            ValidatorVote::sign(&statement, ValidatorId::new(3), &key(10)),
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
        client_sync_certified_public_currency_view(&mut client, NodeId::from_u64(2), &validators),
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

#[test]
fn certified_type_is_not_required_for_network_transport() {
    fn assert_unverified(_: &PublicCurrencyCheckpointProof) {}
    fn assert_certified(_: &CertifiedPublicCurrencyCheckpoint) {}

    let validators = validators();
    let state = SecondState::genesis([], 1).with_reserve(1).unwrap();
    let proof = checkpoint_proof(&state, &validators, 1);
    assert_unverified(&proof);

    let view = second::PublicCurrencyView::new(
        state.public_currency_summary(),
        state.public_currency_states(),
    )
    .unwrap();
    let certified = proof.verify(&view, &validators).unwrap();
    assert_certified(&certified);
}
