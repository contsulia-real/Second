//! Regression checks for the original audit reproductions.
#[path = "../tests/integration/support/mod.rs"]
mod support;

use second::{
    BftDriver, NodeRuntime, Operation, PeerRecord, PreparedTaskBook, QuicServer,
    QuicTransportIdentity, SecondState, StateStore, ValidatorId, ValidatorSigner,
};
use std::time::Duration;

#[tokio::main]
async fn main() {
    #[cfg(windows)]
    degraded_mirror_refuses_signing_rollback();
    let validators = support::validator_set(1, 1..=4);
    let account = support::account(1);
    let tasks = [
        support::verified_task(901, vec![Operation::Issue { account, count: 1 }]),
        support::verified_task(902, vec![Operation::Issue { account, count: 1 }]),
    ];
    let mut stores = Vec::new();
    let mut digests = Vec::new();
    for order in [[0, 1], [1, 0]] {
        let store = StateStore::new(support::temp_base("audit-order"));
        let mut state = SecondState::genesis([account], 1);
        store.initialize(&state, &validators).unwrap();
        let mut book = PreparedTaskBook::new(store.clone()).unwrap();
        for task in &tasks {
            support::allocate_task(&store, &mut state, task, 1, &validators).unwrap();
        }
        for index in order {
            book.prepare(&mut state, &tasks[index], 1, &validators)
                .unwrap();
        }
        digests.push(book.prepared_plan_digest(tasks[0].task_id()).unwrap());
        stores.push(store);
    }
    assert_eq!(digests[0], digests[1]);
    let subject_a = stores[0]
        .prepared_bft_proposal_subject(tasks[0].task_id())
        .unwrap();
    let subject_b = stores[1]
        .prepared_bft_proposal_subject(tasks[0].task_id())
        .unwrap();
    let mut proposer = BftDriver::new(
        ValidatorSigner::new(ValidatorId::new(1), support::key(4), stores[0].clone()),
        stores[0].clone(),
        validators.clone(),
        subject_a.scope().clone(),
    )
    .unwrap();
    let proposal = proposer.create_proposal(&subject_a).unwrap();
    let mut recipient = BftDriver::new(
        ValidatorSigner::new(ValidatorId::new(3), support::key(10), stores[1].clone()),
        stores[1].clone(),
        validators.clone(),
        subject_b.scope().clone(),
    )
    .unwrap();
    recipient
        .accept_proposal(&proposal, &subject_b, None)
        .unwrap();
    println!("PASS: opposite preparation order preserves certified task addresses and digest");

    let base = support::temp_base("audit-bootstrap");
    let store = StateStore::new(&base);
    store
        .initialize(&SecondState::genesis([], 1), &validators)
        .unwrap();
    let runtime = NodeRuntime::load_and_bind("127.0.0.1:0".parse().unwrap(), &store).unwrap();
    let bad_identity = QuicTransportIdentity::generate().unwrap();
    let bad_server = QuicServer::bind("127.0.0.1:0".parse().unwrap(), &bad_identity).unwrap();
    let bad_record = PeerRecord::new(
        bad_server.node_id(),
        bad_server.local_addr().unwrap(),
        bad_identity.certificate_der().to_vec(),
    )
    .unwrap();
    let good_identity = QuicTransportIdentity::generate().unwrap();
    let good_server = QuicServer::bind("127.0.0.1:0".parse().unwrap(), &good_identity).unwrap();
    let good_record = PeerRecord::new(
        good_server.node_id(),
        good_server.local_addr().unwrap(),
        good_identity.certificate_der().to_vec(),
    )
    .unwrap();
    let good_id = good_server.node_id();
    let malicious = tokio::spawn(async move {
        let peer = bad_server.accept().await.unwrap();
        let _request = peer.accept_request().await.unwrap().unwrap();
        std::future::pending::<()>().await;
    });
    let healthy = tokio::spawn(async move {
        let peer = good_server.accept().await.unwrap();
        let request = peer.accept_request().await.unwrap().unwrap();
        request
            .respond(&second::NetworkMessage::Peers {
                records: Vec::new(),
            })
            .await
            .unwrap();
        std::future::pending::<()>().await;
    });
    tokio::time::timeout(
        Duration::from_secs(7),
        runtime.bootstrap(&[bad_record, good_record], 2),
    )
    .await
    .expect("silent peer deadline must permit the next candidate")
    .unwrap();
    assert!(runtime.peer(good_id).is_some());
    println!("PASS: silent peer times out and bootstrap reaches the healthy candidate");
    healthy.abort();
    malicious.abort();
    drop(runtime);
    std::fs::write(support::peer_store_path(&base), b"partial peer cache").unwrap();
    assert!(NodeRuntime::load_and_bind("127.0.0.1:0".parse().unwrap(), &store).is_ok());
    println!("PASS: malformed peer cache does not prevent node startup");
    support::cleanup_node_runtime(store, base);
    for store in stores {
        store.remove_files().unwrap();
    }
}

#[cfg(windows)]
fn degraded_mirror_refuses_signing_rollback() {
    use second::{CURRENT_PROTOCOL_VERSION, ConsensusScope, PublicCurrencyCheckpoint};
    let validators = support::validator_set(1, 1..=4);
    let state = SecondState::genesis([], 1);
    let store = StateStore::new(support::temp_base("audit-mirror"));
    store.initialize(&state, &validators).unwrap();
    let checkpoint =
        PublicCurrencyCheckpoint::new(CURRENT_PROTOCOL_VERSION, 1, state.public_currency_summary());
    let scope = ConsensusScope::PublicCheckpoint {
        validator_set_version: 1,
        epoch: 1,
    };
    let validator_id = ValidatorId::new(1);
    support::mark_bft_finality_ready(
        &store,
        validator_id,
        scope,
        checkpoint.digest(),
        &validators,
        (1..=3).map(|id| (ValidatorId::new(id), support::key((id * 3 + 1) as u8))),
    );
    let before = store.load().unwrap().unwrap().generation;
    let mirror = store.slot_path_for_generation(before);
    let original_permissions = std::fs::metadata(&mirror).unwrap().permissions();
    let mut permissions = original_permissions.clone();
    permissions.set_readonly(true);
    std::fs::set_permissions(&mirror, permissions).unwrap();
    let signer = ValidatorSigner::new(validator_id, support::key(4), store.clone());
    signer
        .sign_public_checkpoint(&checkpoint, &validators)
        .unwrap();
    let committed = store.load().unwrap().unwrap().generation;
    assert_eq!(committed, before + 1);
    std::fs::write(
        store.slot_path_for_generation(committed),
        b"damaged primary",
    )
    .unwrap();
    assert!(store.load().is_err());
    assert!(
        signer
            .sign_public_checkpoint(&checkpoint, &validators)
            .is_err()
    );
    std::fs::set_permissions(&mirror, original_permissions).unwrap();
    println!("PASS: committed signing state cannot roll back to a stale mirror");
    store.remove_files().unwrap();
}
