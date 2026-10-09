//! A waiting storage transaction must not stall independent QUIC requests.
use super::*;
use crate::prepared::tests::{key, temp_store, validator_set};
use crate::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

#[tokio::test]
async fn locked_submission_store_does_not_block_independent_ping() {
    let (store, base) = temp_store();
    let validators = validator_set();
    store
        .initialize(&SecondState::genesis([], 1), &validators)
        .unwrap();
    let runtime = NodeRuntime::bind_loaded(
        "127.0.0.1:0".parse().unwrap(),
        &store,
        store.load().unwrap().unwrap(),
        NodeRuntimeCapabilities::default().with_validator(
            ValidatorRuntimeKeys::new(ValidatorId::new(1), key(3), key(4)),
            ValidatorRuntimeConfig::new(
                AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap(),
                BftTimeoutConfig::new(
                    Duration::from_secs(1),
                    Duration::from_secs(1),
                    Duration::from_secs(1),
                ),
                || 1,
            ),
        ),
    )
    .unwrap();
    let context = runtime.task_submission_context().unwrap();
    let governance_context = runtime.governance_context().unwrap();
    let status_context = runtime.task_status_context().unwrap();
    let identity = QuicTransportIdentity::generate().unwrap();
    let server = Arc::new(QuicServer::bind("127.0.0.1:0".parse().unwrap(), &identity).unwrap());
    let address = server.local_addr().unwrap();
    let node = server.node_id();
    let active_requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let listener_requests = active_requests.clone();
    let listener = tokio::spawn(async move {
        loop {
            let peer = server.accept().await.unwrap();
            let context = context.clone();
            let governance_context = governance_context.clone();
            let status_context = status_context.clone();
            let active = listener_requests.clone();
            tokio::spawn(async move {
                let request = peer.accept_request().await.unwrap().unwrap();
                if let NetworkMessage::Ping { nonce } = request.message() {
                    let response = NetworkMessage::Pong { nonce: *nonce };
                    request.respond(&response).await.unwrap();
                } else if matches!(
                    request.message(),
                    NetworkMessage::LegalTaskStatusQuery { .. }
                ) {
                    let permit =
                        crate::runtime::ActiveConnectionPermit::try_acquire(&active).unwrap();
                    crate::runtime_task_status::serve_legal_task_status_from_request(
                        status_context,
                        request,
                        permit,
                    )
                    .await
                    .unwrap();
                } else if matches!(
                    request.message(),
                    NetworkMessage::StateRecoveryCheckpointSubmit { .. }
                ) {
                    let permit =
                        crate::runtime::ActiveConnectionPermit::try_acquire(&active).unwrap();
                    crate::runtime_governance::serve_governance_request_from_request(
                        governance_context,
                        &peer,
                        request,
                        permit,
                    )
                    .await
                    .unwrap();
                } else {
                    let permit =
                        crate::runtime::ActiveConnectionPermit::try_acquire(&active).unwrap();
                    serve_legal_task_submission_from_request(context, &peer, request, permit)
                        .await
                        .unwrap();
                }
            });
        }
    });
    let client = QuicClient::new(
        "0.0.0.0:0".parse().unwrap(),
        identity.certificate_der(),
        QuicTransportIdentity::generate().unwrap(),
    )
    .unwrap();
    let submission_peer = client.connect(address).await.unwrap();
    let ping_peer = client.connect(address).await.unwrap();
    let governance_peer = client.connect(address).await.unwrap();
    let status_peer = client.connect(address).await.unwrap();
    let task = crate::test_helpers::sign(
        LegalTaskPayload::new(
            TaskId::parse("blocked-storage-submission").unwrap(),
            1,
            None,
            vec![Operation::RegisterAccount {
                account: crate::test_helpers::account(179),
            }],
        ),
        &key(9),
    )
    .unwrap();
    let task_id = task.payload().task_id();
    let status_task = task.clone();
    let held = Arc::new(AtomicBool::new(false));
    let held_by_writer = held.clone();
    let lock_base = base.clone();
    let (locked, ready) = tokio::sync::oneshot::channel();
    let writer = std::thread::spawn(move || {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(lock_base)
            .unwrap();
        file.lock().unwrap();
        held_by_writer.store(true, Ordering::SeqCst);
        locked.send(()).unwrap();
        std::thread::sleep(Duration::from_secs(2));
        held_by_writer.store(false, Ordering::SeqCst);
        file.unlock().unwrap();
    });
    ready.await.unwrap();
    let submission = tokio::spawn(async move {
        let result = client_submit_legal_task(&submission_peer, &task).await;
        submission_peer.close();
        result
    });
    let governance = tokio::spawn(async move {
        let result =
            client_submit_recovery_checkpoint(&governance_peer, ValidatorId::new(1), &key(3), 1)
                .await;
        governance_peer.close();
        result
    });
    let status_query = tokio::spawn(async move {
        let result = client_legal_task_status(&status_peer, &status_task).await;
        status_peer.close();
        result
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let pong = client_ping(&ping_peer, 71).await.unwrap();
    let ping_while_locked = held.load(Ordering::SeqCst);
    let pending_requests = active_requests.load(Ordering::SeqCst);
    assert_eq!(pong, node);
    assert!(
        ping_while_locked,
        "submission, governance or status storage wait blocked the independent network request"
    );
    let accepted = submission.await.unwrap().unwrap();
    let status = status_query.await.unwrap().unwrap();
    assert_eq!(status.task_id, task_id);
    assert!(matches!(
        status.status,
        LegalTaskStatus::Unknown | LegalTaskStatus::Prepared
    ));
    assert!(matches!(
        governance.await.unwrap(),
        Err(NetworkError::GovernanceRejected(
            crate::GovernanceRejection::Busy
        ))
    ));
    writer.join().unwrap();
    assert_eq!(accepted.task_id, task_id);
    assert_eq!(accepted.outcome, LegalTaskSubmissionOutcome::Prepared);
    assert_eq!(pending_requests, 3);
    tokio::time::timeout(Duration::from_secs(1), async {
        while active_requests.load(Ordering::SeqCst) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        store
            .load()
            .unwrap()
            .unwrap()
            .prepared_tasks
            .contains_key(&task_id)
    );
    ping_peer.close();
    client.wait_idle().await;
    listener.abort();
    let _ = listener.await;
    drop(runtime);
    store.remove_files().unwrap();
    let _ = std::fs::remove_file(base.with_extension("lock"));
    let _ = std::fs::remove_file(crate::transport_identity_path(&base));
    let _ = std::fs::remove_file(base.with_extension("transport.lock"));
}
