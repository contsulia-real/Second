//! Lost business-finality messages must not make the original decision unrecoverable.
use std::{path::PathBuf, sync::Arc, time::Duration};

use second::{
    BftConsensusEvent, BftPhase, BftQuorumCertificate, BftStatement, BftTimeoutConfig, BftValue,
    ConsensusScope, FinalityCertificate, LegalTaskPayload, Operation, PreparedTaskBook,
    SecondState, StateStore, TaskId, ValidatorId, ValidatorRuntimeConfig, ValidatorRuntimeKeys,
    ValidatorSigner,
};

use crate::support::{self, key};

struct TestFiles(PathBuf);

impl Drop for TestFiles {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).expect("remove this test's isolated files");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restarted_finality_voters_finish_without_hidden_byzantine_vote() {
    let files = TestFiles(support::temp_base("hidden-finality-recovery"));
    std::fs::create_dir(&files.0).unwrap();
    let validators = support::validator_set(1, 1..=4);
    let account = support::account(230);
    let task = support::sign_task(
        LegalTaskPayload::new(
            TaskId::parse("hidden-finality-recovery").unwrap(),
            1,
            Some(2),
            vec![Operation::RegisterAccount { account }],
        ),
        &key(9),
    )
    .unwrap()
    .verify(&support::authorizers())
    .unwrap();
    let bases = (1..=4)
        .map(|id| files.0.join(format!("node-{id}")))
        .collect::<Vec<_>>();
    let stores = bases.iter().map(StateStore::new).collect::<Vec<_>>();
    for (index, store) in stores.iter().enumerate() {
        let mut state = SecondState::genesis([], 1);
        store.initialize(&state, &validators).unwrap();
        if index < 3 {
            PreparedTaskBook::new(store.clone())
                .unwrap()
                .prepare(&mut state, &task, 1, &validators)
                .unwrap();
        }
    }
    let statement = PreparedTaskBook::new(stores[0].clone())
        .unwrap()
        .prepared_finality_statement(task.task_id())
        .unwrap();
    let scope = ConsensusScope::PreparedTask(task.task_id());
    let value = BftValue::Digest(statement.subject_digest());
    let signers = (1..=3)
        .map(|id| {
            ValidatorSigner::new(
                ValidatorId::new(id),
                key((id * 3 + 1) as u8),
                stores[id as usize - 1].clone(),
            )
        })
        .collect::<Vec<_>>();
    let prevotes = signers
        .iter()
        .map(|signer| {
            signer
                .sign_bft_prevote(scope.clone(), 0, value, &validators, None)
                .unwrap()
        })
        .collect();
    let prevote_qc = BftQuorumCertificate::new(
        BftStatement::new(1, scope.clone(), 0, BftPhase::Prevote, value),
        prevotes,
        &validators,
    )
    .unwrap();
    let precommits = signers
        .iter()
        .map(|signer| {
            signer
                .sign_bft_precommit(scope.clone(), 0, value, &validators, Some(&prevote_qc))
                .unwrap()
        })
        .collect();
    let qc = BftQuorumCertificate::new(
        BftStatement::new(1, scope.clone(), 0, BftPhase::Precommit, value),
        precommits,
        &validators,
    )
    .unwrap();
    let votes = (1..=3)
        .map(|id| {
            stores[id as usize - 1]
                .accept_bft_precommit_qc(ValidatorId::new(id), &qc, &validators)
                .unwrap();
            PreparedTaskBook::new(stores[id as usize - 1].clone())
                .unwrap()
                .sign_prepared_vote(
                    task.task_id(),
                    ValidatorId::new(id),
                    &key((id * 3 + 1) as u8),
                )
                .unwrap()
        })
        .collect();
    // B owns a valid certificate but sends neither its vote nor the certificate.
    FinalityCertificate::new(statement, votes, &validators)
        .unwrap()
        .verify(&validators)
        .unwrap();
    assert_eq!(
        PreparedTaskBook::new(stores[3].clone())
            .unwrap()
            .prepared_count(),
        0
    );
    assert!(
        stores[3]
            .bft_local_state(ValidatorId::new(4), &scope)
            .unwrap()
            .is_none()
    );
    let opposite = support::bft_qc(
        scope.clone(),
        0,
        BftPhase::Precommit,
        BftValue::Digest([88; 32]),
        [1, 2, 3],
        &validators,
    );
    for id in [1_u64, 2] {
        let store = &stores[id as usize - 1];
        let generation = store.load().unwrap().unwrap().generation;
        assert_eq!(
            store
                .accept_bft_precommit_qc(ValidatorId::new(id), &qc, &validators)
                .unwrap(),
            generation
        );
        assert!(
            store
                .accept_bft_precommit_qc(ValidatorId::new(id), &opposite, &validators)
                .is_err()
        );
        assert_eq!(store.load().unwrap().unwrap().generation, generation);
    }
    drop(signers);
    drop(stores);
    drop(qc);
    drop(prevote_qc);

    // Cold stores and fresh runtimes: no pre-crash coordinator or message survives.
    let honest = [1_u64, 2, 4]
        .into_iter()
        .map(|id| {
            let store = StateStore::new(&bases[id as usize - 1]);
            let runtime = Arc::new(support::bind_validator_runtime(
                &store,
                ValidatorRuntimeKeys::new(
                    ValidatorId::new(id),
                    key((id * 3) as u8),
                    key((id * 3 + 1) as u8),
                ),
                ValidatorRuntimeConfig::new(
                    support::authorizers(),
                    BftTimeoutConfig::new(
                        Duration::from_millis(250),
                        Duration::from_millis(250),
                        Duration::from_millis(250),
                    ),
                    || 3,
                ),
            ));
            (runtime, store)
        })
        .collect::<Vec<_>>();
    let records = honest
        .iter()
        .map(|(runtime, _)| support::peer_record(runtime))
        .collect::<Vec<_>>();
    let jobs = honest
        .iter()
        .enumerate()
        .map(|(index, (runtime, _))| {
            support::spawn_node_runtime_with_bootstrap(
                runtime,
                records
                    .iter()
                    .enumerate()
                    .filter(|(other, _)| *other != index)
                    .map(|(_, record)| record.clone())
                    .collect(),
            )
        })
        .collect::<Vec<_>>();
    let result = tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            if honest.iter().all(|(_, store)| {
                store
                    .load()
                    .unwrap()
                    .unwrap()
                    .state
                    .task_succeeded(task.task_id())
                    == Some(true)
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    let events = honest
        .iter()
        .map(|(runtime, _)| runtime.drain_bft_consensus_events().unwrap())
        .collect::<Vec<_>>();
    for job in jobs {
        job.abort();
        let _ = job.await;
    }
    assert!(
        result.is_ok(),
        "honest nodes failed to recover hidden finality after restart: {events:?}"
    );
    for ((id, (_, _)), events) in [1_u64, 2, 4].into_iter().zip(&honest).zip(&events) {
        let cold = StateStore::new(&bases[id as usize - 1])
            .load()
            .unwrap()
            .unwrap();
        assert!(cold.state.has_account(account));
        assert!(
            StateStore::new(&bases[id as usize - 1])
                .bft_local_state(ValidatorId::new(id), &scope)
                .unwrap()
                .is_none()
        );
        let certificate = events
            .iter()
            .find_map(|event| match event {
                BftConsensusEvent::CertifiedPreparedTask {
                    task_id,
                    certificate,
                } if task_id == &task.task_id() => Some(certificate),
                _ => None,
            })
            .expect("actual runtime must emit recovered finality");
        certificate.verify(&validators).unwrap();
        assert_eq!(certificate.statement(), statement);
        assert_eq!(certificate.vote_count(), 3);
        assert!(
            certificate
                .votes()
                .iter()
                .all(|vote| vote.validator_id() != ValidatorId::new(3))
        );
    }
}
