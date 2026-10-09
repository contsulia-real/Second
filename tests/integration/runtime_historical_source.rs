//! Business invalidation of admitted witnesses must finish through original task BFT.
use crate::support::{self, bind_validator_runtime, key, peer_record, temp_base, validator_set};
use second::*;
use std::{sync::Arc, time::Duration};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retired_address_unusable_witnesses_converge_to_certified_abort_without_new_rights() {
    let validators = validator_set(1, 1..=4);
    let alice = support::account(121);
    let bob = support::account(122);
    let charlie = support::account(123);
    let destination = support::payment_address(bob);
    let setup = support::verified_task(
        3200,
        vec![
            Operation::RegisterPaymentAddress {
                address: support::payment_address(alice),
                account: alice,
            },
            Operation::RegisterPaymentAddress {
                address: destination,
                account: bob,
            },
            Operation::RegisterPaymentAddress {
                address: support::payment_address(charlie),
                account: charlie,
            },
            Operation::Issue {
                account: alice,
                count: 1,
            },
        ],
    );
    let transfer = support::verified_task(
        3201,
        vec![Operation::Transfer {
            source: support::payment_address(alice),
            destination,
            amount: 1,
        }],
    );
    let blocker = support::verified_task(
        3202,
        vec![Operation::Transfer {
            source: support::payment_address(alice),
            destination: support::payment_address(charlie),
            amount: 1,
        }],
    );
    let retire = support::verified_task(
        3203,
        vec![Operation::RetirePaymentAddress {
            address: destination,
        }],
    );
    let certify = |statement| {
        support::certificate_from_keys(
            statement,
            &validators,
            (1..=3).map(|id| (ValidatorId::new(id), key((id * 3 + 1) as u8))),
        )
    };
    let mut fixtures = Vec::new();
    for id in 1..=4 {
        let base = temp_base(&format!("historical-retired-witness-{id}"));
        let store = StateStore::new(&base);
        let mut state = SecondState::genesis([alice, bob, charlie], 1);
        store.initialize(&state, &validators).unwrap();
        support::allocate_task(&store, &mut state, &setup, 1, &validators).unwrap();
        let mut book = PreparedTaskBook::new(store.clone()).unwrap();
        book.prepare(&mut state, &setup, 1, &validators).unwrap();
        let certificate = certify(book.prepared_finality_statement(setup.task_id()).unwrap());
        book.commit(&mut state, setup.task_id(), &certificate)
            .unwrap();
        let node = Arc::new(bind_validator_runtime(
            &store,
            ValidatorRuntimeKeys::new(
                ValidatorId::new(id),
                key((id * 3) as u8),
                key((id * 3 + 1) as u8),
            ),
            support::validator_runtime_config(BftTimeoutConfig::new(
                Duration::from_millis(500),
                Duration::from_millis(500),
                Duration::from_millis(500),
            )),
        ));
        let (owned, witness) = if id % 2 == 0 {
            (&transfer, &blocker)
        } else {
            (&blocker, &transfer)
        };
        node.submit_legal_task(owned.signed_task().clone()).unwrap();
        assert_eq!(
            node.submit_legal_task(witness.signed_task().clone())
                .unwrap(),
            LegalTaskSubmissionOutcome::AlreadyPending
        );
        state = store.load().unwrap().unwrap().state;
        book = PreparedTaskBook::new(store.clone()).unwrap();
        book.prepare(&mut state, &retire, 1, &validators).unwrap();
        let certificate = certify(book.prepared_finality_statement(retire.task_id()).unwrap());
        book.commit(&mut state, retire.task_id(), &certificate)
            .unwrap();
        store
            .install_prepared_abort(
                &blocker.task_id(),
                &certify(store.prepared_abort_statement(&blocker.task_id()).unwrap()),
            )
            .unwrap();
        assert_eq!(
            store
                .load()
                .unwrap()
                .unwrap()
                .state
                .payment_address_status(destination),
            Some(PaymentAddressStatus::Retiring)
        );
        drop(node);
        let node = Arc::new(bind_validator_runtime(
            &store,
            ValidatorRuntimeKeys::new(
                ValidatorId::new(id),
                key((id * 3) as u8),
                key((id * 3 + 1) as u8),
            ),
            support::validator_runtime_config(BftTimeoutConfig::new(
                Duration::from_millis(500),
                Duration::from_millis(500),
                Duration::from_millis(500),
            )),
        ));
        fixtures.push((node, store, base));
    }
    let records: Vec<_> = fixtures
        .iter()
        .map(|(node, _, _)| peer_record(node))
        .collect();
    let workers: Vec<_> = fixtures
        .iter()
        .enumerate()
        .map(|(index, (node, _, _))| {
            support::spawn_node_runtime_with_bootstrap(
                node,
                records
                    .iter()
                    .enumerate()
                    .filter(|(other, _)| *other != index)
                    .map(|(_, record)| record.clone())
                    .collect(),
            )
        })
        .collect();
    let completed = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if fixtures.iter().all(|(_, store, _)| {
                store
                    .load()
                    .unwrap()
                    .unwrap()
                    .state
                    .task_cancelled(transfer.task_id())
            }) {
                break;
            }
            assert!(workers.iter().all(|worker| !worker.is_finished()));
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    if completed.is_err() {
        for (index, (node, store, base)) in fixtures.iter().enumerate() {
            let state = store.load().unwrap().unwrap().state;
            let local = store
                .bft_local_state(
                    ValidatorId::new(index as u64 + 1),
                    &ConsensusScope::PreparedTask(transfer.task_id()),
                )
                .unwrap();
            eprintln!(
                "node={} fixture={} cancelled={} peers={:?} round={:?} lock={:?} qc={:?}",
                index + 1,
                base.display(),
                state.task_cancelled(transfer.task_id()),
                node.connected_validator_ids(),
                local.as_ref().map(BftLocalState::round),
                local.as_ref().and_then(BftLocalState::locked_digest),
                local
                    .as_ref()
                    .and_then(BftLocalState::valid_prevote_qc)
                    .map(|qc| qc.statement())
            );
            for event in node.drain_bft_consensus_events().unwrap() {
                if let BftConsensusEvent::Rejected { scope, error, .. } = event {
                    eprintln!(
                        "node={} rejected scope={scope:?} error={error:?}",
                        index + 1
                    );
                }
            }
        }
    }
    assert!(
        completed.is_ok(),
        "known witnesses invalidated by certified retirement did not resolve through original committee BFT"
    );
    for worker in workers {
        worker.abort();
        let _ = worker.await;
    }
    for (node, store, base) in fixtures {
        let cold_store = StateStore::new(&base);
        let cold = cold_store.load().unwrap().unwrap();
        assert!(cold.state.task_cancelled(transfer.task_id()));
        assert_eq!(cold.state.balance(alice), 1);
        assert_eq!(cold.state.balance(bob), 0);
        assert_eq!(cold.state.balance(charlie), 0);
        assert_eq!(cold.state.payment_execution_count(), 0);
        assert_eq!(
            PreparedTaskBook::new(cold_store)
                .unwrap()
                .claimed_currency_count(),
            0
        );
        assert_eq!(cold.validator_set, validators);
        assert!(node.drain_bft_consensus_events().unwrap().iter().any(|event|
            matches!(event, BftConsensusEvent::CertifiedPreparedTask { task_id, certificate }
                if task_id == &transfer.task_id() && certificate.verify(&validators).is_ok())));
        drop(node);
        support::cleanup_node_runtime(store, base);
    }
}
