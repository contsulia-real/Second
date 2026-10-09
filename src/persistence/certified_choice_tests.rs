use crate::prepared::tests::{key, temp_store, validator_set};
use crate::{
    AuthorizerSet, BftPhase, BftQuorumCertificate, BftStatement, BftValue, BftVote, ConsensusScope,
    FinalityCertificate, LegalTaskPayload, Operation, PreparedTaskBook, SecondState, TaskId,
    ValidatorId, ValidatorVote,
};

#[test]
fn opposite_precommit_readiness_and_finalized_choice_are_rejected_in_both_arrival_orders() {
    let validators = validator_set();
    let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
    let task = crate::test_helpers::sign(
        LegalTaskPayload::new(
            TaskId::parse("immutable-finalized-choice").unwrap(),
            1,
            None,
            vec![Operation::RegisterAccount {
                account: crate::test_helpers::account(123),
            }],
        ),
        &key(9),
    )
    .unwrap()
    .verify(&authorizers)
    .unwrap();
    for qc_first in [true, false] {
        let (store, base) = temp_store();
        let mut state = SecondState::genesis([], 1);
        store.initialize(&state, &validators).unwrap();
        let mut book = PreparedTaskBook::new(store.clone()).unwrap();
        book.prepare(&mut state, &task, 1, &validators).unwrap();
        let commit = book.prepared_finality_statement(task.task_id()).unwrap();
        let certificate = FinalityCertificate::new(
            commit,
            (2..=4)
                .map(|id| {
                    ValidatorVote::sign_unchecked(
                        &commit,
                        ValidatorId::new(id),
                        &key((id * 3 + 1) as u8),
                    )
                })
                .collect(),
            &validators,
        )
        .unwrap();
        let abort = store.prepared_abort_statement(&task.task_id()).unwrap();
        let statement = BftStatement::new(
            1,
            ConsensusScope::PreparedTask(task.task_id()),
            0,
            BftPhase::Precommit,
            BftValue::Digest(abort.subject_digest()),
        );
        let qc = BftQuorumCertificate::new(
            statement.clone(),
            (2..=4)
                .map(|id| {
                    BftVote::sign_unchecked(
                        &statement,
                        ValidatorId::new(id),
                        &key((id * 3 + 1) as u8),
                    )
                })
                .collect(),
            &validators,
        )
        .unwrap();
        if qc_first {
            store
                .accept_bft_precommit_qc(ValidatorId::new(1), &qc, &validators)
                .unwrap();
        } else {
            store
                .finalize_prepared_task(&task.task_id(), commit.subject_digest(), &certificate)
                .unwrap();
        }
        let generation = store.load().unwrap().unwrap().generation;
        let result = if qc_first {
            store.finalize_prepared_task(&task.task_id(), commit.subject_digest(), &certificate)
        } else {
            store.accept_bft_precommit_qc(ValidatorId::new(1), &qc, &validators)
        };
        assert!(matches!(
            result,
            Err(crate::PersistenceError::InvalidSnapshot)
        ));
        let cold = crate::StateStore::new(&base).load().unwrap().unwrap();
        assert_eq!(cold.generation, generation);
        assert!(cold.state.business.accounts.is_empty());
        if !qc_first {
            PreparedTaskBook::recover_finalized_from_store(&store).unwrap();
            assert_eq!(
                store
                    .load()
                    .unwrap()
                    .unwrap()
                    .state
                    .task_succeeded(task.task_id()),
                Some(true)
            );
        }
        store.remove_files().unwrap();
        let _ = std::fs::remove_file(base.with_extension("lock"));
    }
}
