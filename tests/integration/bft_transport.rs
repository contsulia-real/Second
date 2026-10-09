use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use second::{
    BftNetworkMessage, BftPhase, BftStatement, BftValue, CURRENT_PROTOCOL_VERSION, NetworkError,
    NetworkMessage, PublicCurrencyCheckpoint, SecondState, StateStore, ValidatorId,
    ValidatorSigner, authenticate_validator_bft_peer, serve_validator_bft_connection,
};

use crate::support::{
    self, bft_qc, certificate_from_keys, key, signed_bft_vote, signed_vote, temp_base,
    validator_set,
};

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
        serve_validator_bft_connection(
            &peer,
            ValidatorId::new(2),
            &key(6),
            &server_validators,
            |sender, message| {
                server_captured.lock().unwrap().push((sender, message));
                std::future::ready(Ok(()))
            },
        )
        .await
    });

    let client = support::quic_client(&certificate);
    let peer = client.connect(address).await.unwrap();
    let bft_peer = authenticate_validator_bft_peer(peer, ValidatorId::new(1), &key(3), &validators)
        .await
        .unwrap();
    assert_eq!(bft_peer.local_validator_id(), ValidatorId::new(1));
    assert_eq!(bft_peer.remote_validator_id(), ValidatorId::new(2));

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
    let finality_statement = checkpoint.finality_statement(validators.version());
    let finality_vote_message = BftNetworkMessage::FinalityVote {
        scope: subject.scope().clone(),
        statement: finality_statement,
        vote: signed_vote(&finality_statement, ValidatorId::new(3), &key(10)),
    };
    let finality_certificate_message = BftNetworkMessage::FinalityCertificate {
        scope: subject.scope().clone(),
        certificate: certificate_from_keys(
            finality_statement,
            &validators,
            [
                (ValidatorId::new(1), key(4)),
                (ValidatorId::new(2), key(7)),
                (ValidatorId::new(3), key(10)),
            ],
        ),
    };

    for message in [
        &proposal_message,
        &vote_message,
        &qc_message,
        &finality_vote_message,
        &finality_certificate_message,
    ] {
        bft_peer.send(message).await.unwrap();
    }
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if captured.lock().unwrap().len() == 5 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("server must consume every framed BFT message before close");
    bft_peer.shutdown().await.unwrap();

    assert_eq!(server_task.await.unwrap().unwrap(), ValidatorId::new(1));
    let received = captured.lock().unwrap();
    assert_eq!(
        received.as_slice(),
        &[
            (ValidatorId::new(1), proposal_message),
            (ValidatorId::new(1), vote_message),
            (ValidatorId::new(1), qc_message),
            (ValidatorId::new(1), finality_vote_message),
            (ValidatorId::new(1), finality_certificate_message),
        ]
    );
    drop(received);
    store.remove_files().unwrap();
}

#[tokio::test]
async fn validator_only_bft_transport_completes_concurrent_messages_without_head_of_line_blocking()
{
    let validators = validator_set(7, 1..=4);
    let (server, certificate) = support::quic_server();
    let address = server.local_addr().unwrap();
    let received = Arc::new(AtomicUsize::new(0));
    let server_received = Arc::clone(&received);
    let server_validators = validators.clone();

    let server_task = tokio::spawn(async move {
        let peer = server.accept().await.unwrap();
        serve_validator_bft_connection(
            &peer,
            ValidatorId::new(2),
            &key(6),
            &server_validators,
            |_sender, _message| {
                server_received.fetch_add(1, Ordering::Relaxed);
                std::future::ready(Ok(()))
            },
        )
        .await
    });

    let client = support::quic_client(&certificate);
    let peer = client.connect(address).await.unwrap();
    let bft_peer = authenticate_validator_bft_peer(peer, ValidatorId::new(1), &key(3), &validators)
        .await
        .unwrap();

    let statement = BftStatement::new(
        validators.version(),
        second::ConsensusScope::PublicCheckpoint {
            validator_set_version: validators.version(),
            epoch: 9,
        },
        0,
        BftPhase::Prevote,
        BftValue::Nil,
    );
    let message = BftNetworkMessage::Vote {
        statement: statement.clone(),
        vote: signed_bft_vote(&statement, ValidatorId::new(1)),
    };

    let sends = (0..12)
        .map(|_| {
            let peer = bft_peer.clone();
            let message = message.clone();
            tokio::spawn(async move { peer.send(&message).await })
        })
        .collect::<Vec<_>>();

    tokio::time::timeout(Duration::from_secs(2), async {
        for send in sends {
            send.await.unwrap().unwrap();
        }
    })
    .await
    .expect("concurrent BFT messages must serialize onto the persistent stream");

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if received.load(Ordering::Relaxed) == 12 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("server must consume every concurrent BFT message before close");

    bft_peer.shutdown().await.unwrap();
    assert_eq!(server_task.await.unwrap().unwrap(), ValidatorId::new(1));
    assert_eq!(received.load(Ordering::Relaxed), 12);
}

#[tokio::test]
async fn validator_only_bft_transport_rejects_forged_server_validator_identity() {
    let validators = validator_set(7, 1..=4);
    let (server, certificate) = support::quic_server();
    let address = server.local_addr().unwrap();

    let server_task = tokio::spawn(async move {
        let peer = server.accept().await.unwrap();
        let request = peer.accept_request().await.unwrap().unwrap();
        assert!(matches!(
            request.message(),
            NetworkMessage::BftAuthenticate {
                validator_id,
                ..
            } if *validator_id == ValidatorId::new(1)
        ));
        request
            .respond(&NetworkMessage::BftAuthenticated {
                validator_id: ValidatorId::new(2),
                validator_set_version: 7,
                signature: [0; 64],
            })
            .await
            .unwrap();
    });

    let client = support::quic_client(&certificate);
    let peer = client.connect(address).await.unwrap();
    assert!(matches!(
        authenticate_validator_bft_peer(peer, ValidatorId::new(1), &key(3), &validators).await,
        Err(NetworkError::BftUnauthorized)
    ));
    server_task.await.unwrap();
}

#[tokio::test]
async fn validator_only_bft_transport_rejects_forged_identity_authentication() {
    let validators = validator_set(7, 1..=4);
    let (server, certificate) = support::quic_server();
    let address = server.local_addr().unwrap();
    let server_validators = validators.clone();

    let server_task = tokio::spawn(async move {
        let peer = server.accept().await.unwrap();
        serve_validator_bft_connection(
            &peer,
            ValidatorId::new(2),
            &key(6),
            &server_validators,
            |_sender, _message| async {
                panic!("forged authentication must not reach BFT message handler")
            },
        )
        .await
    });

    let client = support::quic_client(&certificate);
    let peer = client.connect(address).await.unwrap();
    let response = peer
        .exchange(&NetworkMessage::BftAuthenticate {
            validator_id: ValidatorId::new(1),
            validator_set_version: 7,
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
