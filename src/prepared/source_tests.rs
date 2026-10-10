use super::source::PreparedTaskSource;
use super::tests::{key, temp_store, validator_set};
use super::*;
use crate::currency::Currency;
use crate::payment::PaymentAddressRecord;
use crate::{AuthorizerSet, CurrencyAddress, CurrencyRole, LegalTaskPayload, PaymentAddressStatus};

#[test]
fn frozen_source_preserves_remote_selection_and_rejects_tampering_without_writes() {
    run_frozen_source(false, false, false);
}

#[test]
fn blocked_frozen_candidate_cannot_sign_or_release_old_claims_and_acquires_after_certified_abort() {
    run_frozen_source(true, false, false);
}

#[test]
fn commit_qc_protects_unowned_alternative_without_granting_signing_or_resource_rights() {
    run_frozen_source(true, true, false);
}

#[test]
fn finality_protects_unowned_alternative_without_prior_local_qc_or_resource_rights() {
    run_frozen_source(true, true, true);
}

fn run_frozen_source(blocked: bool, protected: bool, finality: bool) {
    let validators = validator_set();
    let alice = crate::test_helpers::account(61);
    let bob = crate::test_helpers::account(62);
    let source = PaymentAddress::from_bytes([61; 32]);
    let destination = PaymentAddress::from_bytes([62; 32]);
    let mut initial = SecondState::genesis([alice, bob], 4);
    for (address, account) in [(source, alice), (destination, bob)] {
        initial.business.payment_addresses.insert(
            address,
            PaymentAddressRecord {
                account,
                status: PaymentAddressStatus::Active,
            },
        );
    }
    for number in 1..=3 {
        let address = CurrencyAddress::new(number);
        initial.business.currencies.insert(
            address,
            Currency {
                address,
                role: CurrencyRole::Circulation,
                owner: Some(if number == 3 { bob } else { alice }),
            },
        );
    }
    let authorizers = AuthorizerSet::new(
        CURRENT_PROTOCOL_VERSION,
        [key(9).verifying_key().to_bytes()],
    )
    .unwrap();
    let task = |name| {
        crate::test_helpers::sign(
            LegalTaskPayload::new(
                TaskId::parse(name).unwrap(),
                CURRENT_PROTOCOL_VERSION,
                None,
                vec![Operation::Transfer {
                    source,
                    destination,
                    amount: 1,
                }],
            ),
            &key(9),
        )
        .unwrap()
        .verify(&authorizers)
        .unwrap()
    };
    let blocker = task("frozen-blocker");
    let transfer = task("frozen-transfer");
    let holder = task(if protected {
        "frozen-a-holder"
    } else {
        "frozen-zzz-holder"
    });
    let (remote, remote_base) = temp_store();
    let (local, local_base) = temp_store();
    remote.initialize(&initial, &validators).unwrap();
    local.initialize(&initial, &validators).unwrap();
    let mut remote_state = initial.clone();
    let mut remote_book = PreparedTaskBook::new(remote.clone()).unwrap();
    remote_book
        .prepare(&mut remote_state, &blocker, 1, &validators)
        .unwrap();
    remote_book
        .prepare(&mut remote_state, &transfer, 1, &validators)
        .unwrap();
    remote_book.cancel(blocker.task_id()).unwrap();
    let prepared = remote
        .load_prepared_tasks()
        .unwrap()
        .remove(&transfer.task_id())
        .unwrap();
    let digest = prepared.plan_digest().unwrap();
    let bytes = prepared.encode_source().unwrap();
    let decoded = PreparedTaskSource::decode(&bytes).unwrap();
    assert_eq!(decoded.task, *transfer.signed_task());
    assert_eq!(
        decoded.selections,
        vec![
            vec![CurrencyAddress::new(2)]
                .into_iter()
                .collect::<crate::AddressRanges>()
        ]
    );
    assert!(PreparedTaskSource::decode(&bytes[..bytes.len() - 1]).is_none());
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(PreparedTaskSource::decode(&trailing).is_none());

    let mut state = initial;
    let mut book = PreparedTaskBook::new(local.clone()).unwrap();
    let generation = local.load().unwrap().unwrap().generation;
    // The default local choice (1) is valid business, but is not the remote plan (2).
    assert!(matches!(
        book.prepare_expected_plan(
            &mut state,
            &transfer,
            1,
            &validators,
            digest,
            &[vec![CurrencyAddress::new(1)]
                .into_iter()
                .collect::<crate::AddressRanges>()]
        ),
        Err(PreparationError::PreparedPlanDigestMismatch { .. })
    ));
    assert!(matches!(
        book.prepare_expected_plan(
            &mut state,
            &transfer,
            1,
            &validators,
            digest,
            &[vec![CurrencyAddress::new(3)]
                .into_iter()
                .collect::<crate::AddressRanges>()]
        ),
        Err(PreparationError::Execution(
            ExecutionError::CurrencyNotOwned(_)
        ))
    ));
    assert_eq!(local.load().unwrap().unwrap().generation, generation);
    assert_eq!(state.bound_request_digest(transfer.task_id()), None);
    assert_eq!(book.claimed_currency_count(), 0);
    book.prepare(&mut state, &transfer, 1, &validators).unwrap();
    if blocked {
        book.prepare(&mut state, &holder, 1, &validators).unwrap();
    }
    let original_digest = book.prepared_plan_digest(transfer.task_id()).unwrap();
    let signer = ValidatorSigner::new(ValidatorId::new(1), key(4), local.clone());
    signer
        .sign_bft_prevote(
            crate::ConsensusScope::PreparedTask(transfer.task_id()),
            0,
            crate::BftValue::Digest(original_digest),
            &validators,
            None,
        )
        .unwrap();
    let mut book = PreparedTaskBook::new(local.clone()).unwrap();
    let contention = book
        .admit_frozen_variant(
            &mut state,
            &transfer,
            &validators,
            digest,
            &decoded.selections,
        )
        .unwrap();
    if blocked {
        let losers = book
            .admit_contention(&mut state, &transfer, &validators, contention.unwrap())
            .unwrap();
        if protected {
            assert_eq!(losers, vec![transfer.task_id()]);
            let statement = crate::BftStatement::new(
                validators.version(),
                crate::ConsensusScope::PreparedTask(transfer.task_id()),
                0,
                crate::BftPhase::Prevote,
                crate::BftValue::Digest(digest),
            );
            let qc = crate::BftQuorumCertificate::new(
                statement.clone(),
                (2..=4)
                    .map(|id| {
                        crate::BftVote::sign_unchecked(
                            &statement,
                            ValidatorId::new(id),
                            &key((id * 3 + 1) as u8),
                        )
                    })
                    .collect(),
                &validators,
            )
            .unwrap();
            let final_statement =
                FinalityStatement::new(CURRENT_PROTOCOL_VERSION, validators.version(), digest);
            let certificate = FinalityCertificate::new(
                final_statement,
                (2..=4)
                    .map(|id| {
                        ValidatorVote::sign_unchecked(
                            &final_statement,
                            ValidatorId::new(id),
                            &key((id * 3 + 1) as u8),
                        )
                    })
                    .collect(),
                &validators,
            )
            .unwrap();
            let reconcile = || {
                if finality {
                    local.reconcile_prepared_commit_finality(
                        ValidatorId::new(1),
                        &transfer.task_id(),
                        &certificate,
                    )
                } else {
                    local.reconcile_prepared_commit_qc(ValidatorId::new(1), &qc)
                }
            };
            assert_eq!(reconcile().unwrap(), vec![holder.task_id()]);
            let snapshot = local.load().unwrap().unwrap();
            assert!(!snapshot.prepared_tasks[&transfer.task_id()].conflict_abort);
            assert!(snapshot.prepared_tasks[&holder.task_id()].conflict_abort);
            assert!(
                snapshot.prepared_tasks[&transfer.task_id()]
                    .candidate(digest)
                    .unwrap()
                    .is_some_and(|candidate| !candidate.commit_authorized)
            );
            let generation = snapshot.generation;
            reconcile().unwrap();
            assert_eq!(local.load().unwrap().unwrap().generation, generation);
        } else {
            assert_eq!(losers, vec![holder.task_id()]);
        }
        let generation = local.load().unwrap().unwrap().generation;
        let statement =
            FinalityStatement::new(CURRENT_PROTOCOL_VERSION, validators.version(), digest);
        assert!(
            signer
                .sign_prepared_task(transfer.task_id(), &statement, &validators)
                .is_err()
        );
        assert!(
            signer
                .sign_bft_prevote(
                    crate::ConsensusScope::PreparedTask(transfer.task_id()),
                    1,
                    crate::BftValue::Digest(digest),
                    &validators,
                    None
                )
                .is_err()
        );
        assert_eq!(local.load().unwrap().unwrap().generation, generation);
        let mut restarted = PreparedTaskBook::new(local.clone()).unwrap();
        assert_eq!(restarted.claimed_currency_count(), 2);
        let statement = local.prepared_abort_statement(&holder.task_id()).unwrap();
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
        restarted
            .abort_certified(&mut state, holder.task_id(), &certificate)
            .unwrap();
        assert_eq!(restarted.claimed_currency_count(), 1);
        assert!(
            restarted
                .admit_frozen_variant(
                    &mut state,
                    &transfer,
                    &validators,
                    digest,
                    &decoded.selections
                )
                .unwrap()
                .is_none()
        );
        book = restarted;
    } else {
        assert!(contention.is_none());
    }
    assert_eq!(book.claimed_currency_count(), 2);
    drop(book);
    let mut book = PreparedTaskBook::new(local.clone()).unwrap();
    assert_eq!(book.claimed_currency_count(), 2);
    let recovered = local
        .load_prepared_tasks()
        .unwrap()
        .remove(&transfer.task_id())
        .unwrap();
    assert!(recovered.candidate(original_digest).unwrap().is_some());
    assert!(recovered.candidate(digest).unwrap().is_some());
    let snapshot = local.load().unwrap().unwrap();
    let mut invalid_permissions = snapshot.prepared_tasks.clone();
    invalid_permissions
        .get_mut(&transfer.task_id())
        .unwrap()
        .commit_authorized = false;
    for variant in &mut invalid_permissions
        .get_mut(&transfer.task_id())
        .unwrap()
        .variants
    {
        variant.commit_authorized = false;
    }
    assert!(matches!(
        local.replace_prepared_tasks(&snapshot.prepared_tasks, &invalid_permissions),
        Err(PersistenceError::InvalidSnapshot)
    ));
    assert_eq!(
        local.load().unwrap().unwrap().generation,
        snapshot.generation
    );
    let handoff = crate::persistence::TaskHandoff::capture(&snapshot).unwrap();
    handoff.covers(&snapshot).unwrap();
    assert_eq!(handoff.plans.len(), 2);
    let mut reordered = snapshot.clone();
    let selected = reordered
        .prepared_tasks
        .get_mut(&transfer.task_id())
        .unwrap();
    let other = if selected.plan_digest().unwrap() == digest {
        original_digest
    } else {
        digest
    };
    selected.select_candidate(other).unwrap();
    let same_handoff = crate::persistence::TaskHandoff::capture(&reordered).unwrap();
    assert_eq!(handoff.digest().unwrap(), same_handoff.digest().unwrap());
    handoff.covers(&reordered).unwrap();
    let statement = FinalityStatement::new(CURRENT_PROTOCOL_VERSION, validators.version(), digest);
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
    book.commit(&mut state, transfer.task_id(), &certificate)
        .unwrap();
    assert_eq!(state.balance(alice), 1);
    assert_eq!(state.balance(bob), 2);
    assert_eq!(
        state
            .business
            .currencies
            .get(&CurrencyAddress::new(1))
            .unwrap()
            .owner,
        Some(alice)
    );
    assert_eq!(
        state
            .business
            .currencies
            .get(&CurrencyAddress::new(2))
            .unwrap()
            .owner,
        Some(bob)
    );
    for (store, base) in [(remote, remote_base), (local, local_base)] {
        store.remove_files().unwrap();
        let _ = std::fs::remove_file(base.with_extension("lock"));
    }
}

#[test]
fn leak_repair_source_preserves_selected_reserve_and_rejects_circulating_substitute() {
    let validators = validator_set();
    let alice = crate::test_helpers::account(63);
    let mut initial = SecondState::genesis([alice], 5);
    for number in 1..=4 {
        let address = CurrencyAddress::new(number);
        initial.business.currencies.insert(
            address,
            Currency {
                address,
                role: if number <= 2 {
                    CurrencyRole::Reserve
                } else {
                    CurrencyRole::Circulation
                },
                owner: (number > 2).then_some(alice),
            },
        );
    }
    let authorizers = AuthorizerSet::new(
        CURRENT_PROTOCOL_VERSION,
        [key(9).verifying_key().to_bytes()],
    )
    .unwrap();
    let task = |name, number| {
        let mut task = crate::test_helpers::sign(
            LegalTaskPayload::new(
                TaskId::parse(name).unwrap(),
                CURRENT_PROTOCOL_VERSION,
                None,
                vec![Operation::LeakRepair {
                    leaked: vec![CurrencyAddress::new(number)],
                }],
            ),
            &key(9),
        )
        .unwrap();
        task.add_account_signature(&key(63)).unwrap();
        task.verify(&authorizers).unwrap()
    };
    let blocker = task("reserve-blocker", 3);
    let repair = task("reserve-repair", 4);
    let (remote, remote_base) = temp_store();
    let (local, local_base) = temp_store();
    remote.initialize(&initial, &validators).unwrap();
    for (task, start) in [(&blocker, 5), (&repair, 6)] {
        let allocation = crate::CurrencyAllocation::new(task, validators.version(), start).unwrap();
        let statement = allocation.finality_statement();
        let votes = (1..=3)
            .map(|id| {
                ValidatorVote::sign_unchecked(
                    &statement,
                    ValidatorId::new(id),
                    &key((id * 3 + 1) as u8),
                )
            })
            .collect();
        remote
            .install_currency_allocation(
                &allocation,
                &FinalityCertificate::new(statement, votes, &validators).unwrap(),
            )
            .unwrap();
    }
    let initial = remote.load().unwrap().unwrap().state;
    local.initialize(&initial, &validators).unwrap();
    let mut remote_state = initial.clone();
    let mut remote_book = PreparedTaskBook::new(remote.clone()).unwrap();
    remote_book
        .prepare(&mut remote_state, &blocker, 1, &validators)
        .unwrap();
    remote_book
        .prepare(&mut remote_state, &repair, 1, &validators)
        .unwrap();
    remote_book.cancel(blocker.task_id()).unwrap();
    let prepared = remote
        .load_prepared_tasks()
        .unwrap()
        .remove(&repair.task_id())
        .unwrap();
    let digest = prepared.plan_digest().unwrap();
    let decoded = PreparedTaskSource::decode(&prepared.encode_source().unwrap()).unwrap();
    assert_eq!(
        decoded.selections,
        vec![
            vec![CurrencyAddress::new(2)]
                .into_iter()
                .collect::<crate::AddressRanges>()
        ]
    );
    let mut state = initial;
    let mut book = PreparedTaskBook::new(local.clone()).unwrap();
    let generation = local.load().unwrap().unwrap().generation;
    assert!(matches!(
        book.prepare_expected_plan(
            &mut state,
            &repair,
            1,
            &validators,
            digest,
            &[vec![CurrencyAddress::new(3)]
                .into_iter()
                .collect::<crate::AddressRanges>()]
        ),
        Err(PreparationError::Execution(
            ExecutionError::CurrencyNotCirculation(_)
        ))
    ));
    assert_eq!(local.load().unwrap().unwrap().generation, generation);
    assert_eq!(book.claimed_currency_count(), 0);
    book.prepare_expected_plan(
        &mut state,
        &repair,
        1,
        &validators,
        digest,
        &decoded.selections,
    )
    .unwrap();
    let statement = book.prepared_finality_statement(repair.task_id()).unwrap();
    let votes = (1..=3)
        .map(|id| {
            ValidatorVote::sign_unchecked(
                &statement,
                ValidatorId::new(id),
                &key((id * 3 + 1) as u8),
            )
        })
        .collect();
    book.commit(
        &mut state,
        repair.task_id(),
        &FinalityCertificate::new(statement, votes, &validators).unwrap(),
    )
    .unwrap();
    assert!(!state.currency_exists(CurrencyAddress::new(4)));
    assert_eq!(
        state
            .business
            .currencies
            .get(&CurrencyAddress::new(1))
            .unwrap()
            .role,
        CurrencyRole::Reserve
    );
    assert_eq!(
        state
            .business
            .currencies
            .get(&CurrencyAddress::new(2))
            .unwrap()
            .owner,
        Some(alice)
    );
    assert_eq!(
        state
            .business
            .currencies
            .get(&CurrencyAddress::new(6))
            .unwrap()
            .role,
        CurrencyRole::Reserve
    );
    assert_eq!(state.next_currency_address(), 7);
    for (store, base) in [(remote, remote_base), (local, local_base)] {
        store.remove_files().unwrap();
        let _ = std::fs::remove_file(base.with_extension("lock"));
    }
}
