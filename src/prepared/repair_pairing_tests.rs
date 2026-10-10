use super::source::PreparedTaskSource;
use super::tests::{key, temp_store, validator_set};
use super::*;
use crate::reserve_sampling::ReserveSamplingSeed;
use crate::{
    AddressRange, AuthorizerSet, CurrencyAddress, CurrencyRole, LegalTask, LegalTaskPayload,
};

#[test]
fn multi_owner_repair_pairing_survives_frozen_source_recovery_and_commit() {
    let alice = crate::test_helpers::account(61);
    let bob = crate::test_helpers::account(62);
    let validators = validator_set();
    let certify = |statement: FinalityStatement| {
        let votes = (1..=3)
            .map(|id| {
                ValidatorVote::sign_unchecked(
                    &statement,
                    ValidatorId::new(id),
                    &key((id * 3 + 1) as u8),
                )
            })
            .collect();
        FinalityCertificate::new(statement, votes, &validators).unwrap()
    };
    let mut initial = SecondState::genesis([alice, bob], 1)
        .with_reserve(16)
        .unwrap();
    for index in 0..8 {
        initial.business.currencies.set_range(
            AddressRange::new(CurrencyAddress::new(17 + index), 1).unwrap(),
            CurrencyRole::Circulation,
            Some(if index % 2 == 0 { alice } else { bob }),
        );
    }
    initial.protocol.next_currency_address = 25;
    let request = |name| {
        let mut task = LegalTask::sign(
            LegalTaskPayload::new(
                TaskId::parse(name).unwrap(),
                1,
                None,
                vec![
                    Operation::LeakRepair {
                        leaked: (17..21).map(CurrencyAddress::new).collect(),
                    },
                    Operation::LeakRepair {
                        leaked: (21..25).map(CurrencyAddress::new).collect(),
                    },
                ],
            ),
            &key(9),
        )
        .unwrap();
        task.add_account_signature(&key(61)).unwrap();
        task.add_account_signature(&key(62)).unwrap();
        task.verify(&AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap())
            .unwrap()
    };
    let task = request("paired-repair");
    let other_request = request("different-paired-repair");
    let (original, original_base) = temp_store();
    original.initialize(&initial, &validators).unwrap();
    let allocation = crate::CurrencyAllocation::new(&task, 1, 25).unwrap();
    original
        .install_currency_allocation(&allocation, &certify(allocation.finality_statement()))
        .unwrap();
    let initial = original.load().unwrap().unwrap().state;
    let (restored, restored_base) = temp_store();
    restored.initialize(&initial, &validators).unwrap();
    let mut original_state = initial.clone();
    let mut original_book = PreparedTaskBook::new(original.clone()).unwrap();
    original_book
        .prepare(&mut original_state, &task, 1, &validators)
        .unwrap();
    let plan = original
        .load_prepared_tasks()
        .unwrap()
        .remove(&task.task_id())
        .unwrap();
    let source = PreparedTaskSource::decode(&plan.encode_source().unwrap()).unwrap();
    let digest = plan.plan_digest().unwrap();
    let pairing = ReserveSamplingSeed::new(task.request_digest(), 0).pair(&source.selections[0]);
    assert_ne!(
        pairing,
        ReserveSamplingSeed::new(other_request.request_digest(), 0).pair(&source.selections[0])
    );
    assert_ne!(
        pairing,
        ReserveSamplingSeed::new(task.request_digest(), 1).pair(&source.selections[0])
    );
    let mut restored_state = initial;
    let mut restored_book = PreparedTaskBook::new(restored.clone()).unwrap();
    restored_book
        .prepare_expected_plan(
            &mut restored_state,
            &task,
            1,
            &validators,
            digest,
            &source.selections,
        )
        .unwrap();
    assert_eq!(
        restored_book.prepared_plan_digest(task.task_id()).unwrap(),
        digest
    );
    // Reload the persisted plan before committing; neither selection nor pairing is recomputed by default sampling.
    drop(restored_book);
    let mut restored_book = PreparedTaskBook::new(restored.clone()).unwrap();
    let certificate = certify(
        original_book
            .prepared_finality_statement(task.task_id())
            .unwrap(),
    );
    original_book
        .commit(&mut original_state, task.task_id(), &certificate)
        .unwrap();
    restored_book
        .commit(&mut restored_state, task.task_id(), &certificate)
        .unwrap();
    assert_eq!(original_state.business, restored_state.business);
    for (index, selection) in source.selections.iter().enumerate() {
        for (offset, address) in ReserveSamplingSeed::new(task.request_digest(), index as u64)
            .pair(selection)
            .into_iter()
            .enumerate()
        {
            assert_eq!(
                restored_state
                    .business
                    .currencies
                    .get(&address)
                    .unwrap()
                    .owner,
                Some(if offset % 2 == 0 { alice } else { bob })
            );
        }
    }
    for (store, base) in [(original, original_base), (restored, restored_base)] {
        store.remove_files().unwrap();
        let _ = std::fs::remove_file(base.with_extension("lock"));
    }
}
