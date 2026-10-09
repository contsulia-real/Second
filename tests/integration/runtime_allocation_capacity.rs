//! Durable allocation backlog must respect the bounded candidate window.
use crate::support::{self, bind_validator_runtime, key, temp_base, validator_set};
use second::{
    BftConsensusRuntimeError, BftPhase, BftValue, CurrencyAllocation, NodeRuntimeError, Operation,
    SecondState, StateStore, ValidatorId, ValidatorRuntimeKeys, ValidatorSigner,
};
use std::{sync::Arc, time::Duration};

#[tokio::test]
async fn allocation_backlog_beyond_candidate_window_survives_restart_and_drains() {
    let base = temp_base("allocation-candidate-backlog");
    let store = StateStore::new(&base);
    let account = support::account(97);
    let validators = validator_set(1, [1]);
    store
        .initialize(&SecondState::genesis([account], 1), &validators)
        .unwrap();
    let bind = || {
        bind_validator_runtime(
            &store,
            ValidatorRuntimeKeys::new(ValidatorId::new(1), key(3), key(4)),
            support::default_validator_runtime_config(),
        )
    };
    let runtime = bind();
    let tasks = (0..65)
        .map(|index| {
            support::verified_task(2000 + index, vec![Operation::Issue { account, count: 1 }])
        })
        .collect::<Vec<_>>();
    let mut limited = 0;
    for task in &tasks {
        match runtime.submit_legal_task(task.signed_task().clone()) {
            Ok(_) => {}
            Err(NodeRuntimeError::BftConsensus(
                BftConsensusRuntimeError::PendingFutureMessagesFull(_),
            )) => limited += 1,
            Err(error) => panic!("unexpected submission failure: {error:?}"),
        }
    }
    assert_eq!(
        limited, 1,
        "exercise the actual 64-candidate admission boundary"
    );
    drop(runtime);
    // A previous arrival order can leave the last queued candidate locked.
    // Its proof and body must be restored ahead of the first 64 ordinary entries.
    let locked = CurrencyAllocation::new(tasks.last().unwrap(), validators.version(), 1).unwrap();
    let proof = support::bft_qc(
        locked.scope(),
        0,
        BftPhase::Prevote,
        BftValue::Digest(locked.digest()),
        [1],
        &validators,
    );
    ValidatorSigner::new(ValidatorId::new(1), key(4), store.clone())
        .sign_bft_precommit(
            locked.scope(),
            0,
            BftValue::Digest(locked.digest()),
            &validators,
            Some(&proof),
        )
        .unwrap();
    let runtime = Arc::new(bind());
    let worker = support::spawn_node_runtime(&runtime);
    let completed = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            assert!(
                !worker.is_finished(),
                "bounded admission killed restart recovery"
            );
            let state = store.load().unwrap().unwrap().state;
            if tasks
                .iter()
                .all(|task| state.task_succeeded(task.task_id()) == Some(true))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    worker.abort();
    let _ = worker.await;
    if completed.is_err() {
        support::allocation_diagnostics::report(
            &runtime,
            &store,
            &base,
            ValidatorId::new(1),
            tasks.iter().map(|task| task.task_id()),
        );
    }
    assert!(completed.is_ok(), "persisted backlog failed to drain");
    let state = store.load().unwrap().unwrap().state;
    assert_eq!(state.next_currency_address(), 66);
    assert_eq!(state.balance(account), 65);
    drop(runtime);
    support::cleanup_node_runtime(store, base);
}
