use super::*;
use crate::prepared::tests::{key, temp_store, validator_set};
use crate::*;
use std::time::Duration;

#[test]
fn submission_reloads_changed_prepared_evidence_and_revalidates_changed_business() {
    let validators = validator_set();
    let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
    let task = |name: &str, account| {
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
    let first_account = crate::test_helpers::account(171);
    let second_account = crate::test_helpers::account(172);
    let first = task("submission-concurrent-finality", first_account);
    let second = task("submission-independent", second_account);
    let (store, base) = temp_store();
    let mut state = SecondState::genesis([], 1);
    store.initialize(&state, &validators).unwrap();
    let mut book = PreparedTaskBook::new(store.clone()).unwrap();
    book.prepare(&mut state, &first, 1, &validators).unwrap();
    let before_finality = store.load_shared().unwrap().unwrap();
    let statement = book.prepared_finality_statement(first.task_id()).unwrap();
    let votes = (1..=3)
        .map(|id| {
            ValidatorVote::sign_unchecked(
                &statement,
                ValidatorId::new(id),
                &key((id * 3 + 1) as u8),
            )
        })
        .collect();
    let certificate = FinalityCertificate::new(statement, votes, &validators).unwrap();
    store
        .finalize_prepared_task(
            &first.task_id(),
            book.prepared_plan_digest(first.task_id()).unwrap(),
            &certificate,
        )
        .unwrap();
    let runtime = ValidatorBftRuntime::new(
        ValidatorRuntimeKeys::new(ValidatorId::new(1), key(3), key(4)),
        ValidatorRuntimeConfig::new(
            authorizers.clone(),
            BftTimeoutConfig::new(
                Duration::from_secs(1),
                Duration::from_secs(1),
                Duration::from_secs(1),
            ),
            || 1,
        ),
        store.clone(),
        validators.clone(),
        std::iter::empty(),
    )
    .unwrap();
    let context = LegalTaskSubmissionContext::new(store.clone(), runtime);
    // An actual concurrent finality write changed only the prepared evidence.
    // The stale attempt must fail its CAS, then the submission must reload.
    assert!(matches!(
        context.submit_snapshot(second.signed_task(), &second, &before_finality),
        Err(NodeRuntimeError::Preparation(
            PreparationError::Persistence(PersistenceError::StalePreparedTasks)
        ))
    ));
    assert_eq!(
        context
            .submit_verified(second.signed_task(), &second, before_finality)
            .unwrap(),
        LegalTaskSubmissionOutcome::Prepared,
    );
    let accepted = store.load_shared().unwrap().unwrap();
    assert_eq!(
        accepted.prepared_tasks[&first.task_id()]
            .finality_votes
            .as_deref(),
        Some(certificate.votes())
    );
    assert!(accepted.prepared_tasks.contains_key(&second.task_id()));
    assert!(!accepted.state.business.accounts.contains(&second_account));

    PreparedTaskBook::recover_finalized_from_store(&store).unwrap();
    let duplicate = task("submission-after-business-change", first_account);
    assert!(matches!(
        context.submit_snapshot(duplicate.signed_task(), &duplicate, &accepted),
        Err(NodeRuntimeError::Preparation(
            PreparationError::Persistence(PersistenceError::StaleState)
        ))
    ));
    assert!(matches!(
        context.submit_verified(duplicate.signed_task(), &duplicate, accepted),
        Err(NodeRuntimeError::Preparation(PreparationError::Execution(ExecutionError::AccountAlreadyExists(value)))) if value == first_account
    ));
    let conflicting_request = task("submission-independent", first_account);
    let generation = store.load().unwrap().unwrap().generation;
    assert!(
        context
            .submit(conflicting_request.signed_task().clone())
            .is_err()
    );
    let cold = StateStore::new(&base).load().unwrap().unwrap();
    assert_eq!(cold.generation, generation);
    assert_eq!(cold.state.task_succeeded(first.task_id()), Some(true));
    assert!(cold.prepared_tasks.contains_key(&second.task_id()));
    assert!(!cold.state.business.accounts.contains(&second_account));
    drop(context);
    store.remove_files().unwrap();
}
