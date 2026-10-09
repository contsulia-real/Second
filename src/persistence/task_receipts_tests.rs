use super::*;
use crate::prepared::tests::{key, temp_store, validator_set};
use crate::{
    AuthorizerSet, LegalTaskPayload, Operation, PreparedTaskBook, ValidatorId, ValidatorVote,
};

#[test]
fn committed_receipts_evict_by_commit_order_and_reject_forged_recovery_proofs() {
    let validators = validator_set();
    let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
    let (store, base) = temp_store();
    let mut state = SecondState::genesis([], 1);
    store.initialize(&state, &validators).unwrap();
    let mut first = None;
    let mut last = None;
    for index in 0..=MAX_TASK_RECEIPTS {
        // Lexical order is deliberately opposite to commit order.
        let task_id = TaskId::parse(&format!("receipt-{:03}", MAX_TASK_RECEIPTS - index)).unwrap();
        let task = crate::test_helpers::sign(
            LegalTaskPayload::new(
                task_id.clone(),
                1,
                None,
                vec![Operation::RegisterAccount {
                    account: crate::test_helpers::account(index as u8),
                }],
            ),
            &key(9),
        )
        .unwrap()
        .verify(&authorizers)
        .unwrap();
        let mut book = PreparedTaskBook::new(store.clone()).unwrap();
        book.prepare(&mut state, &task, 1, &validators).unwrap();
        let statement = book.prepared_finality_statement(task_id.clone()).unwrap();
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
        book.commit(&mut state, task_id.clone(), &certificate)
            .unwrap();
        if index == 0 {
            first = Some(task_id.clone());
        }
        last = Some(task_id);
    }
    let cold = crate::StateStore::new(&base).load().unwrap().unwrap();
    assert_eq!(cold.task_receipts.len(), MAX_TASK_RECEIPTS);
    let first = first.unwrap();
    let last = last.unwrap();
    assert!(!cold.task_receipts.contains_key(&first));
    assert_eq!(cold.state.task_succeeded(first), Some(true));
    assert!(cold.task_receipts.contains_key(&last));
    assert_eq!(
        PreparedTaskBook::new(store.clone())
            .unwrap()
            .claimed_currency_count(),
        0
    );
    let mut forged = cold.clone();
    let mut plan = forged.task_receipts[&last].plan.clone();
    let statement = forged.task_receipts[&last]
        .certificate()
        .unwrap()
        .statement();
    plan.finality_votes = Some(
        (1..=3)
            .map(|id| ValidatorVote::sign_unchecked(&statement, ValidatorId::new(id), &key(99)))
            .collect(),
    );
    forged.task_receipts.insert(
        last,
        Arc::new(TaskReceipt::new(cold.generation, plan, None).unwrap()),
    );
    let guard = store.lock().unwrap();
    assert!(matches!(
        store.write_local_metadata_unlocked(&forged),
        Err(PersistenceError::InvalidSnapshot)
    ));
    drop(guard);
    assert_eq!(store.load().unwrap().unwrap().generation, cold.generation);
    // A declared oversized body is rejected before reading or allocating it.
    let mut oversized = Vec::new();
    push_len(&mut oversized, 1).unwrap();
    push_len(&mut oversized, MAX_TASK_RECEIPT_BYTES + 1).unwrap();
    assert!(matches!(
        decode(&mut Decoder::new(&oversized)),
        Err(PersistenceError::InvalidSnapshot)
    ));
    store.remove_files().unwrap();
    let _ = std::fs::remove_file(base.with_extension("lock"));
}
