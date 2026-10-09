//! Reuse the installation regression's certified fixture for actual node transfer.
use super::*;
use crate::prepared::tests::key;
use crate::*;
use std::{sync::Arc, time::Duration};

pub(crate) async fn fetch_from_current_member(
    store: &StateStore,
    certified: &CertifiedValidatorSetTransition,
    trusted: &PersistedNodeState,
    authorizers: &AuthorizerSet,
    expected: &[u8],
) -> Vec<u8> {
    assert!(expected.len() > MAX_STATE_RECOVERY_CHUNK_SIZE as usize);
    store.activate_validator_set_transition(certified).unwrap();
    // The provider advances business; its live state cannot substitute the
    // immutable baseline that the previous committee actually certified.
    let advanced_account = crate::test_helpers::account(185);
    let task = crate::test_helpers::sign(
        LegalTaskPayload::new(
            TaskId::parse("provider-after-handoff-baseline").unwrap(),
            1,
            None,
            vec![Operation::RegisterAccount {
                account: advanced_account,
            }],
        ),
        &key(9),
    )
    .unwrap()
    .verify(authorizers)
    .unwrap();
    let snapshot = store.load().unwrap().unwrap();
    let mut state = snapshot.state;
    let next = snapshot.validator_set;
    let mut book = PreparedTaskBook::new(store.clone()).unwrap();
    book.prepare(&mut state, &task, 1, &next).unwrap();
    let statement = book.prepared_finality_statement(task.task_id()).unwrap();
    let certificate = FinalityCertificate::new(
        statement,
        (1..=3)
            .map(|id| {
                ValidatorVote::sign_unchecked(
                    &statement,
                    ValidatorId::new(id),
                    &key((id * 3 + 1) as u8),
                )
            })
            .collect(),
        &next,
    )
    .unwrap();
    book.commit(&mut state, task.task_id(), &certificate)
        .unwrap();
    let runtime = Arc::new(
        NodeRuntime::bind_loaded(
            "127.0.0.1:0".parse().unwrap(),
            store,
            store.load().unwrap().unwrap(),
            NodeRuntimeCapabilities::default().with_validator(
                ValidatorRuntimeKeys::new(ValidatorId::new(1), key(3), key(4)),
                ValidatorRuntimeConfig::new(
                    authorizers.clone(),
                    BftTimeoutConfig::new(
                        Duration::from_secs(1),
                        Duration::from_secs(1),
                        Duration::from_secs(1),
                    ),
                    || 3,
                ),
            ),
        )
        .unwrap(),
    );
    let running = Arc::clone(&runtime);
    let worker = tokio::spawn(async move { running.run(&[]).await });
    let client = QuicClient::new(
        "0.0.0.0:0".parse().unwrap(),
        runtime.transport_certificate_der(),
        QuicTransportIdentity::generate().unwrap(),
    )
    .unwrap();
    let peer = client
        .connect_expected(runtime.local_addr().unwrap(), runtime.node_id())
        .await
        .unwrap();
    let proof = client_validator_set_transition_proof(&peer, trusted.validator_set.version())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        proof.encode_bytes().unwrap(),
        ValidatorSetTransitionProof::from_certified(certified)
            .encode_bytes()
            .unwrap()
    );
    // Proof-only sessions deliberately cannot serve private state. Authenticate
    // body requests on a fresh connection after verifying the public proof.
    peer.close();
    let peer = client
        .connect_expected(runtime.local_addr().unwrap(), runtime.node_id())
        .await
        .unwrap();
    let digest = proof.source().task_handoff_digest().unwrap();
    assert_eq!(
        peer.exchange(&NetworkMessage::GetStateRecoveryChunk {
            validator_id: ValidatorId::new(5),
            checkpoint_digest: digest,
            offset: 0,
            limit: MAX_STATE_RECOVERY_CHUNK_SIZE,
            signature: [0; 64],
        })
        .await
        .unwrap(),
        NetworkMessage::StateRecoveryDenied
    );
    // A valid checkpoint-domain signature is not a handoff authorization.
    let signature = key(15)
        .sign(&chunk_auth_bytes(
            STATE_RECOVERY_AUTH_DOMAIN,
            peer.channel_binding().unwrap(),
            ValidatorId::new(5),
            digest,
            0,
            MAX_STATE_RECOVERY_CHUNK_SIZE,
        ))
        .to_bytes();
    assert_eq!(
        peer.exchange(&NetworkMessage::GetStateRecoveryChunk {
            validator_id: ValidatorId::new(5),
            checkpoint_digest: digest,
            offset: 0,
            limit: MAX_STATE_RECOVERY_CHUNK_SIZE,
            signature,
        })
        .await
        .unwrap(),
        NetworkMessage::StateRecoveryDenied
    );
    let body =
        client_fetch_validator_handoff(&peer, ValidatorId::new(5), &key(15), &proof, trusted)
            .await
            .unwrap();
    assert_eq!(body, expected);
    assert!(
        store
            .load()
            .unwrap()
            .unwrap()
            .state
            .business
            .accounts
            .contains(&advanced_account)
    );
    assert!(
        store
            .load()
            .unwrap()
            .unwrap()
            .recovery_checkpoint_proof
            .is_none()
    );
    assert!(!worker.is_finished());
    peer.close();
    worker.abort();
    let _ = worker.await;
    drop(runtime);
    for extension in ["transport", "transport.lock", "peers"] {
        let _ = std::fs::remove_file(store.base_path().with_extension(extension));
    }
    body
}
