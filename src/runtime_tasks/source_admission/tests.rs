use crate::prepared::tests::{key, temp_store, validator_set};
use crate::*;
use std::time::Duration;

mod unusable;

#[tokio::test]
async fn downloaded_source_reloads_concurrent_finality_and_revalidates_business_changes() {
    let validators = validator_set();
    let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
    let first_account = crate::test_helpers::account(191);
    let second_account = crate::test_helpers::account(192);
    let request = |name: &str, account| {
        crate::test_helpers::sign(
            LegalTaskPayload::new(
                TaskId::parse(name).unwrap(),
                1,
                None,
                vec![Operation::RegisterAccount { account }],
            ),
            &key(9),
        )
        .unwrap()
        .verify(&authorizers)
        .unwrap()
    };
    let first = request("source-concurrent-finality", first_account);
    let second = request("source-independent", second_account);
    let duplicate = request("source-changed-business", first_account);
    let late = request(
        "source-after-business-commit",
        crate::test_helpers::account(193),
    );
    let (provider, _) = temp_store();
    let mut provider_state = SecondState::genesis([], 1);
    provider.initialize(&provider_state, &validators).unwrap();
    let mut provider_book = PreparedTaskBook::new(provider.clone()).unwrap();
    for task in [&second, &duplicate, &late] {
        provider_book
            .prepare(&mut provider_state, task, 1, &validators)
            .unwrap();
    }
    let second_digest = provider_book
        .prepared_plan_digest(second.task_id())
        .unwrap();
    let duplicate_digest = provider_book
        .prepared_plan_digest(duplicate.task_id())
        .unwrap();
    let late_digest = provider_book.prepared_plan_digest(late.task_id()).unwrap();
    let (store, base) = temp_store();
    let mut state = SecondState::genesis([], 1);
    store.initialize(&state, &validators).unwrap();
    let mut book = PreparedTaskBook::new(store.clone()).unwrap();
    book.prepare(&mut state, &first, 1, &validators).unwrap();
    let before_finality = store.load_shared().unwrap().unwrap();
    let statement = book.prepared_finality_statement(first.task_id()).unwrap();
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
        &validators,
    )
    .unwrap();
    store
        .finalize_prepared_task(
            &first.task_id(),
            book.prepared_plan_digest(first.task_id()).unwrap(),
            &certificate,
        )
        .unwrap();
    let runtime = NodeRuntime::bind_loaded(
        "127.0.0.1:0".parse().unwrap(),
        &store,
        store.load().unwrap().unwrap(),
        NodeRuntimeCapabilities::default().with_validator(
            ValidatorRuntimeKeys::new(ValidatorId::new(1), key(3), key(4)),
            ValidatorRuntimeConfig::new(
                authorizers,
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
    let generation = store.load_shared().unwrap().unwrap().generation;
    assert!(matches!(
        runtime.install_prepared_source_snapshot(1, second_digest, &second, &[], &before_finality),
        Err(BftConsensusRuntimeError::Preparation(
            PreparationError::Persistence(PersistenceError::StalePreparedTasks)
        ))
    ));
    assert_eq!(store.load_shared().unwrap().unwrap().generation, generation);
    runtime
        .install_verified_prepared_source(1, second_digest, &second, &[], before_finality)
        .unwrap();
    let before_commit = store.load_shared().unwrap().unwrap();
    assert_eq!(
        before_commit.prepared_tasks[&first.task_id()]
            .finality_votes
            .as_deref(),
        Some(certificate.votes())
    );
    PreparedTaskBook::recover_finalized_from_store(&store).unwrap();
    let generation = store.load_shared().unwrap().unwrap().generation;
    let result = runtime.install_verified_prepared_source(
        1,
        duplicate_digest,
        &duplicate,
        &[],
        before_commit.clone(),
    );
    assert!(
        matches!(
            &result,
            Err(BftConsensusRuntimeError::Preparation(
                PreparationError::Execution(ExecutionError::AccountAlreadyExists(account))
            )) if *account == first_account
        ),
        "changed business must be revalidated: {result:?}"
    );
    assert_eq!(store.load_shared().unwrap().unwrap().generation, generation);
    runtime
        .install_verified_prepared_source(1, late_digest, &late, &[], before_commit)
        .unwrap();
    let cold = StateStore::new(&base).load().unwrap().unwrap();
    assert_eq!(cold.state.task_succeeded(first.task_id()), Some(true));
    assert!(cold.prepared_tasks.contains_key(&second.task_id()));
    assert!(cold.prepared_tasks.contains_key(&late.task_id()));
    assert!(!cold.prepared_tasks.contains_key(&duplicate.task_id()));
    assert!(!cold.state.business.accounts.contains(&second_account));
    drop(runtime);
    provider.remove_files().unwrap();
    store.remove_files().unwrap();
}
