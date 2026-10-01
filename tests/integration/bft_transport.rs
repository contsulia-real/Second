use std::sync::{Arc, Mutex};

use second::{
    BftNetworkMessage, BftPhase, BftStatement, BftValue, CURRENT_PROTOCOL_VERSION, NetworkError,
    NetworkMessage, PublicCurrencyCheckpoint, SecondState, StateStore, ValidatorId,
    ValidatorSigner, authenticate_validator_bft_peer, serve_validator_bft_connection,
};

use crate::support::{self, bft_qc, key, signed_bft_vote, temp_base, validator_set};

#[tokio::test]
async fn validator_only_bft_transport_carries_proposals_votes_and_qcs() {
    let validators = validator_set(7, 1..=4);
    let base = temp_base("bft-transport-messages");
    let store = StateStore::new(&base);
    let state = SecondState::genesis([], 1).with_reserve(1).unwrap();
    store.initialize(&state, &validators).unwrap();
    let checkpoint =
        PublicCurrencyCheckpoint::new(CURRENT_PROTOCOL_VERSION, 9, state.public_currency_summary());
    let subject = store
        .public_checkpoint_bft_proposal_subject(&checkpoint)
        .unwrap();
    let proposal = ValidatorSigner::new(ValidatorId::new(1), key(4), store.clone())
        .sign_bft_proposal(&subject, 0, &validators)
        .unwrap();
    let (server, certificate) = support::quic_server();
    let address = server.local_addr().unwrap();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let server_captured = Arc::clone(&captured);
    let server_validators = validators.clone();

    let server_task = tokio::spawn(async move {
        let peer = server.accept().await.unwrap();
        serve_validator_bft_connection(&peer, &server_validators, |sender, message| {
            server_captured.lock().unwrap().push((sender, message));
            Ok(())
        })
        .await
    });

    let client = support::quic_client(&certificate);
    let peer = client.connect(address).await.unwrap();
    let bft_peer = authenticate_validator_bft_peer(peer, ValidatorId::new(1), &key(3), &validators)
        .await
        .unwrap();

    let proposal_message = BftNetworkMessage::Proposal {
        proposal,
        unlock_certificate: None,
    };
    let statement = BftStatement::new(
        validators.version(),
        subject.scope().clone(),
        0,
        BftPhase::Prevote,
        BftValue::Digest(subject.digest()),
    );
    let vote = signed_bft_vote(&statement, ValidatorId::new(1));
    let vote_message = BftNetworkMessage::Vote {
        statement: statement.clone(),
        vote,
    };
    let qc_message = BftNetworkMessage::QuorumCertificate(bft_qc(
        subject.scope().clone(),
        0,
        BftPhase::Prevote,
        BftValue::Digest(subject.digest()),
        [1, 2, 3],
        &validators,
    ));

    for message in [&proposal_message, &vote_message, &qc_message] {
        bft_peer.send(message).await.unwrap();
    }
    bft_peer.close();

    assert_eq!(server_task.await.unwrap().unwrap(), ValidatorId::new(1));
    let received = captured.lock().unwrap();
    assert_eq!(
        received.as_slice(),
        &[
            (ValidatorId::new(1), proposal_message),
            (ValidatorId::new(1), vote_message),
            (ValidatorId::new(1), qc_message),
        ]
    );
    drop(received);
    store.remove_files().unwrap();
}

#[tokio::test]
async fn validator_only_bft_transport_rejects_forged_identity_authentication() {
    let validators = validator_set(7, 1..=4);
    let (server, certificate) = support::quic_server();
    let address = server.local_addr().unwrap();
    let server_validators = validators.clone();

    let server_task = tokio::spawn(async move {
        let peer = server.accept().await.unwrap();
        serve_validator_bft_connection(&peer, &server_validators, |_sender, _message| {
            panic!("forged authentication must not reach BFT message handler")
        })
        .await
    });

    let client = support::quic_client(&certificate);
    let peer = client.connect(address).await.unwrap();
    let response = peer
        .exchange(&NetworkMessage::BftAuthenticate {
            validator_id: ValidatorId::new(1),
            signature: [0; 64],
        })
        .await
        .unwrap();
    assert_eq!(response, NetworkMessage::BftDenied);
    assert_eq!(
        server_task.await.unwrap(),
        Err(NetworkError::BftUnauthorized)
    );
    peer.close();
}
