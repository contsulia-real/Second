use std::time::{SystemTime, UNIX_EPOCH};

use ed25519_dalek::SigningKey;

use super::*;
use crate::{
    AccountAddress, AuthorizerSet, FinalityCertificate, LegalTask, LegalTaskPayload, Operation,
    PreparationOutcome, TaskId, ValidatorCredential, ValidatorId, ValidatorSet, ValidatorVote,
};

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn validator_set() -> ValidatorSet {
    ValidatorSet::new(
        1,
        (1..=4).map(|id| {
            ValidatorCredential::new(
                ValidatorId::new(id),
                key((id * 3) as u8).verifying_key().to_bytes(),
                key((id * 3 + 1) as u8).verifying_key().to_bytes(),
                key((id * 3 + 2) as u8).verifying_key().to_bytes(),
            )
            .unwrap()
        }),
    )
    .unwrap()
}

fn temp_store() -> (StateStore, std::path::PathBuf) {
    let unique = format!(
        "second-prepared-finality-recovery-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must be after unix epoch")
            .as_nanos(),
    );
    let base = std::env::temp_dir().join(unique);
    (StateStore::new(&base), base)
}

#[test]
fn durable_finality_recovers_commit_after_crash_boundary() {
    let validators = validator_set();
    let account = AccountAddress::from_bytes([41; 32]);
    let (store, base) = temp_store();
    store
        .initialize(&SecondState::genesis([account], 1), &validators)
        .unwrap();

    let authorizer = key(9);
    let authorizers = AuthorizerSet::new(
        CURRENT_PROTOCOL_VERSION,
        [authorizer.verifying_key().to_bytes()],
    )
    .unwrap();
    let task_id = TaskId::parse("finality-recovery").unwrap();
    let signed = LegalTask::sign(
        LegalTaskPayload::new(
            task_id.clone(),
            CURRENT_PROTOCOL_VERSION,
            None,
            vec![Operation::Issue { account, count: 1 }],
        ),
        &authorizer,
    )
    .unwrap();
    let verified = signed.verify(&authorizers).unwrap();

    let persisted = store.load().unwrap().unwrap();
    let mut state = persisted.state;
    let mut book = PreparedTaskBook::new(store.clone()).unwrap();
    assert_eq!(
        book.prepare(&mut state, &verified, 1, &validators).unwrap(),
        PreparationOutcome::Prepared
    );
    let digest = book.prepared_plan_digest(task_id.clone()).unwrap();
    let statement = book.prepared_finality_statement(task_id.clone()).unwrap();
    let votes = (1..=3)
        .map(|id| {
            ValidatorVote::sign_unchecked(
                &statement,
                ValidatorId::new(id),
                &key((id * 3 + 1) as u8),
            )
        })
        .collect::<Vec<_>>();
    let certificate = FinalityCertificate::new(statement, votes, &validators).unwrap();

    store
        .finalize_prepared_task(&task_id, digest, &certificate)
        .unwrap();

    let finalized = store.load().unwrap().unwrap();
    assert_eq!(finalized.state.current_supply(), 0);
    assert!(finalized.prepared_tasks.contains_key(&task_id));
    drop(book);

    PreparedTaskBook::recover_finalized_from_store(&store).unwrap();

    let recovered = store.load().unwrap().unwrap();
    assert_eq!(recovered.state.current_supply(), 1);
    assert!(!recovered.prepared_tasks.contains_key(&task_id));

    store.remove_files().unwrap();
    let _ = std::fs::remove_file(base.with_extension("lock"));
}
