use crate::currency::Currency;
use crate::prepared::tests::{key, temp_store, validator_set};
use crate::*;

#[test]
fn single_repair_delta_contains_only_retired_and_new_identities_after_restart() {
    let alice = crate::test_helpers::account(1);
    let bob = crate::test_helpers::account(2);
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
    let mut state = SecondState::genesis([alice, bob], 1)
        .with_reserve(2)
        .unwrap();
    for (value, owner) in [(3, alice), (4, bob)] {
        let address = CurrencyAddress::new(value);
        state.business.currencies.insert(
            address,
            Currency {
                address,
                role: CurrencyRole::Circulation,
                owner: Some(owner),
            },
        );
    }
    state.protocol.next_currency_address = 5;
    let base = PublicCurrencyView::new(
        state.public_currency_summary(),
        state.public_currency_states(),
    )
    .unwrap();
    // Both role and owner boundaries disappear in the public projection.
    assert_eq!(
        base.states,
        vec![PublicCurrencyState {
            start: CurrencyAddress::new(1),
            len: 4,
            occupied: true
        }]
    );
    let reserve_before = state
        .public_currency_state(CurrencyAddress::new(1))
        .unwrap();
    let (store, _) = temp_store();
    store.initialize(&state, &validators).unwrap();
    let checkpoint = PublicCurrencyCheckpoint::new(1, 1, base.summary.clone());
    store
        .attach_certified_checkpoint(
            &CertifiedPublicCurrencyCheckpoint::new(
                checkpoint.clone(),
                certify(checkpoint.finality_statement(1)).votes().to_vec(),
                &validators,
            )
            .unwrap(),
        )
        .unwrap();
    let mut task = LegalTask::sign(
        LegalTaskPayload::new(
            TaskId::parse("single-private-repair").unwrap(),
            1,
            None,
            vec![Operation::LeakRepair {
                leaked: vec![CurrencyAddress::new(3)],
            }],
        ),
        &key(9),
    )
    .unwrap();
    task.add_account_signature(&key(1)).unwrap();
    let task = task
        .verify(&AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap())
        .unwrap();
    let allocation = CurrencyAllocation::new(&task, 1, 5).unwrap();
    store
        .install_currency_allocation(&allocation, &certify(allocation.finality_statement()))
        .unwrap();
    state = store.load().unwrap().unwrap().state;
    let mut book = PreparedTaskBook::new(store.clone()).unwrap();
    book.prepare(&mut state, &task, 1, &validators).unwrap();
    let certificate = certify(book.prepared_finality_statement(task.task_id()).unwrap());
    book.commit(&mut state, task.task_id(), &certificate)
        .unwrap();
    assert_eq!(
        state
            .public_currency_state(CurrencyAddress::new(1))
            .unwrap(),
        reserve_before
    );
    // The historical baseline must survive disk decoding, not just the write cache.
    let reopened = StateStore::new(store.base_path());
    let checkpoint = PublicCurrencyCheckpoint::new(1, 2, state.public_currency_summary());
    reopened
        .attach_certified_checkpoint(
            &CertifiedPublicCurrencyCheckpoint::new(
                checkpoint.clone(),
                certify(checkpoint.finality_statement(1)).votes().to_vec(),
                &validators,
            )
            .unwrap(),
        )
        .unwrap();
    let snapshot = reopened.load().unwrap().unwrap();
    let delta = snapshot.latest_public_delta.unwrap();
    assert_eq!(
        delta.changes(),
        &[
            PublicCurrencyDeltaChange::Retire(
                AddressRange::new(CurrencyAddress::new(3), 1).unwrap()
            ),
            PublicCurrencyDeltaChange::Upsert(PublicCurrencyState {
                start: CurrencyAddress::new(5),
                len: 1,
                occupied: true
            }),
        ]
    );
    let restored = PublicCurrencyDelta::decode_bytes(&delta.encode_bytes().unwrap())
        .unwrap()
        .apply(1, &base)
        .unwrap();
    assert_eq!(restored.states, state.public_currency_states());
    assert_eq!(restored.summary, state.public_currency_summary());
    let mut invalid = restored.summary.clone();
    invalid.reserve_count = invalid.occupied_count + 1;
    assert_eq!(
        PublicCurrencyView::new(invalid, restored.states.clone()),
        Err(PublicStateError::SummaryMismatch)
    );
    let mut invalid = restored.summary.clone();
    invalid.state_digest[0] ^= 1;
    assert_eq!(
        PublicCurrencyView::new(invalid, restored.states),
        Err(PublicStateError::SummaryMismatch)
    );
    store.remove_files().unwrap();
}
