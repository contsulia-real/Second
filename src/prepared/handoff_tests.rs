use super::tests::{key, temp_store, validator_set};
use super::*;
use crate::{
    AuthorizerSet, CertifiedStateRecoveryCheckpoint, CertifiedValidatorSetTransition,
    LegalTaskPayload, StateRecoveryCheckpoint, StateRecoveryPayload, ValidatorId,
    ValidatorSetTransition, ValidatorVote,
};

fn task(name: &str, account: AccountAddress) -> VerifiedLegalTask {
    let authorizers = AuthorizerSet::new(
        CURRENT_PROTOCOL_VERSION,
        [key(9).verifying_key().to_bytes()],
    )
    .unwrap();
    crate::test_helpers::sign(
        LegalTaskPayload::new(
            TaskId::parse(name).unwrap(),
            CURRENT_PROTOCOL_VERSION,
            None,
            vec![Operation::RegisterAccount { account }],
        ),
        &key(9),
    )
    .unwrap()
    .verify(&authorizers)
    .unwrap()
}

fn votes(statement: &FinalityStatement) -> Vec<ValidatorVote> {
    (1..=3)
        .map(|id| {
            ValidatorVote::sign_unchecked(statement, ValidatorId::new(id), &key((id * 3 + 1) as u8))
        })
        .collect()
}

fn transition(store: &StateStore) -> ValidatorSetTransition {
    let snapshot = store.load().unwrap().unwrap();
    let next = ValidatorSet::new(
        snapshot.validator_set.version() + 1,
        (1..=4).map(|id| {
            snapshot
                .validator_set
                .credential(ValidatorId::new(id))
                .unwrap()
                .clone()
        }),
    )
    .unwrap();
    ValidatorSetTransition::new(
        CURRENT_PROTOCOL_VERSION,
        &snapshot.validator_set,
        &snapshot.validator_registry,
        next,
        vec![],
        vec![],
        snapshot.state.next_currency_address(),
    )
    .unwrap()
}

#[test]
fn certified_historical_contention_survives_membership_and_business_changes() {
    let validators = validator_set();
    let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
    let account = crate::test_helpers::account(161);
    let holder = task("historical-contention-b-holder", account);
    let waiting = task("historical-contention-a-waiting", account);
    let (source, source_base) = temp_store();
    let mut state = SecondState::genesis([], 1);
    source.initialize(&state, &validators).unwrap();
    let mut book = PreparedTaskBook::new(source.clone()).unwrap();
    book.prepare(&mut state, &holder, 1, &validators).unwrap();
    let verified = book
        .verify_local_contention(&state, &waiting, 1, &validators)
        .unwrap();
    let digest = verified.prepared.plan_digest().unwrap();
    book.admit_contention(&mut state, &waiting, &validators, verified)
        .unwrap();
    let trusted = source.load().unwrap().unwrap();
    let transition = source
        .prepare_validator_set_transition(transition(&source))
        .unwrap();
    let body = transition.handoff.as_ref().unwrap().encode().unwrap();
    let statement = transition.finality_statement();
    let certified =
        CertifiedValidatorSetTransition::new(transition, votes(&statement), &validators).unwrap();
    let proof = crate::ValidatorSetTransitionProof::from_certified(&certified);
    let (target, target_base) = temp_store();
    target
        .install_validator_handoff_baseline(&proof, &body, &trusted, &authorizers)
        .unwrap();
    let snapshot = target.load().unwrap().unwrap();
    let mut state = snapshot.state;
    let next = snapshot.validator_set;
    let mut book = PreparedTaskBook::new(target.clone()).unwrap();
    let unrelated = task(
        "historical-contention-later",
        crate::test_helpers::account(162),
    );
    book.prepare(&mut state, &unrelated, 1, &next).unwrap();
    let statement = book
        .prepared_finality_statement(unrelated.task_id())
        .unwrap();
    let certificate = FinalityCertificate::new(statement, votes(&statement), &next).unwrap();
    book.commit(&mut state, unrelated.task_id(), &certificate)
        .unwrap();
    let before = target.load().unwrap().unwrap().generation;
    let unlisted = task("historical-contention-unlisted", account);
    let unlisted_witness = book
        .verify_local_contention(&state, &unlisted, 1, &validators)
        .unwrap();
    assert!(matches!(
        book.admit_contention(&mut state, &unlisted, &validators, unlisted_witness),
        Err(PreparationError::TaskOriginMismatch {
            expected: 2,
            actual: 1
        })
    ));
    assert_eq!(target.load().unwrap().unwrap().generation, before);
    let verified = book
        .verify_contention(&state, &waiting, 1, &validators, digest, &[])
        .unwrap();
    assert!(verified.blockers.contains(&holder.task_id()));
    book.admit_contention(&mut state, &waiting, &validators, verified)
        .expect("exact certified historical witness must use current persistence authority");
    let cold = StateStore::new(&target_base).load().unwrap().unwrap();
    let witness = &cold.prepared_tasks[&waiting.task_id()];
    assert_eq!(witness.validator_set_version, 1);
    assert!(!witness.commit_authorized);
    assert_eq!(witness.plan_digest().unwrap(), digest);
    assert_eq!(cold.validator_set, next);
    assert_eq!(cold.state.task_succeeded(unrelated.task_id()), Some(true));
    assert!(!cold.validator_safety_ready);
    assert!(cold.validator_vote_locks.is_empty());
    assert!(cold.bft_local_states.is_empty());
    // The witness keeps no resource rights while its inherited blocker remains.
    // Original-committee Abort releases the fence; the same body then acquires
    // rights and commits under its original certificate without rolling back V2.
    let abort = target.prepared_abort_statement(&holder.task_id()).unwrap();
    let certificate = FinalityCertificate::new(abort, votes(&abort), &validators).unwrap();
    target
        .install_prepared_abort(&holder.task_id(), &certificate)
        .unwrap();
    let mut state = target.load().unwrap().unwrap().state;
    let mut book = PreparedTaskBook::new(target.clone()).unwrap();
    book.prepare_expected_plan(&mut state, &waiting, 1, &validators, digest, &[])
        .unwrap();
    let statement = book.prepared_finality_statement(waiting.task_id()).unwrap();
    let certificate = FinalityCertificate::new(statement, votes(&statement), &validators).unwrap();
    book.commit(&mut state, waiting.task_id(), &certificate)
        .unwrap();
    let completed = StateStore::new(&target_base).load().unwrap().unwrap();
    assert_eq!(
        completed.state.task_succeeded(waiting.task_id()),
        Some(true)
    );
    assert!(completed.state.task_cancelled(holder.task_id()));
    assert_eq!(
        completed.state.task_succeeded(unrelated.task_id()),
        Some(true)
    );
    assert_eq!(completed.validator_set, next);
    assert!(!completed.validator_safety_ready);
    assert!(completed.validator_vote_locks.is_empty());
    source.remove_files().unwrap();
    target.remove_files().unwrap();
    for base in [source_base, target_base] {
        let _ = std::fs::remove_file(base.with_extension("lock"));
    }
}

#[test]
fn handoff_admission_closes_new_tasks_durably_and_rejects_omission_without_writes() {
    let validators = validator_set();
    let (store, _base) = temp_store();
    let mut state = SecondState::genesis([], 1);
    store.initialize(&state, &validators).unwrap();
    let mut book = PreparedTaskBook::new(store.clone()).unwrap();
    let old = task("handoff-old", crate::test_helpers::account(151));
    book.prepare(&mut state, &old, 1, &validators).unwrap();
    let bare = transition(&store);
    let statement = bare.finality_statement();
    let missing =
        CertifiedValidatorSetTransition::new(bare.clone(), votes(&statement), &validators).unwrap();
    let generation = store.load().unwrap().unwrap().generation;
    assert_eq!(
        store.activate_validator_set_transition(&missing),
        Err(PersistenceError::StalePreparedTasks)
    );
    assert_eq!(store.load().unwrap().unwrap().generation, generation);
    let hydrated = store.prepare_validator_set_transition(bare).unwrap();
    let target = crate::runtime_consensus_target::ValidatorConsensusTarget::ValidatorSetTransition(
        hydrated.clone(),
    );
    store.admit_governance(&target).unwrap();
    let snapshot = store.load().unwrap().unwrap();
    let mut restarted = PreparedTaskBook::new(store.clone()).unwrap();
    let mut state = snapshot.state;
    let new = task("handoff-new", crate::test_helpers::account(152));
    assert_eq!(
        restarted.prepare(&mut state, &new, 1, &validators),
        Err(PreparationError::Persistence(
            PersistenceError::TaskAdmissionClosed {
                validator_set_version: 1
            }
        ))
    );
    assert_eq!(
        store.load().unwrap().unwrap().generation,
        snapshot.generation
    );
    assert!(
        store
            .load()
            .unwrap()
            .unwrap()
            .prepared_tasks
            .contains_key(&old.task_id())
    );
    // The sealed candidate already covers this task. Its legitimate terminal
    // result may advance the business baseline before membership certification.
    let abort = store.prepared_abort_statement(&old.task_id()).unwrap();
    restarted
        .abort_certified(
            &mut state,
            old.task_id(),
            &FinalityCertificate::new(abort, votes(&abort), &validators).unwrap(),
        )
        .unwrap();
    let statement = hydrated.finality_statement();
    let certified =
        CertifiedValidatorSetTransition::new(hydrated, votes(&statement), &validators).unwrap();
    store.activate_validator_set_transition(&certified).unwrap();
    assert_eq!(store.load().unwrap().unwrap().validator_set.version(), 2);
}

#[tokio::test]
async fn certified_handoff_shared_recovery_fences_resources_without_inheriting_commit_rights() {
    let validators = validator_set();
    let (store, _base) = temp_store();
    let mut state = SecondState::genesis([], 1);
    store.initialize(&state, &validators).unwrap();
    let account = crate::test_helpers::account(153);
    let old = task("handoff-owner", account);
    PreparedTaskBook::new(store.clone())
        .unwrap()
        .prepare(&mut state, &old, 1, &validators)
        .unwrap();
    let transition = store
        .prepare_validator_set_transition(transition(&store))
        .unwrap();
    let statement = transition.finality_statement();
    store
        .activate_validator_set_transition(
            &CertifiedValidatorSetTransition::new(transition, votes(&statement), &validators)
                .unwrap(),
        )
        .unwrap();
    let snapshot = store.load().unwrap().unwrap();
    let original = StateRecoveryPayload::from_persisted(&snapshot).unwrap();
    let encoded = original.encode_bytes().unwrap();
    let payload = StateRecoveryPayload::decode_bytes(&encoded).unwrap();
    let mut forged = encoded.clone();
    *forged.last_mut().unwrap() ^= 1;
    assert!(StateRecoveryPayload::decode_bytes(&forged).is_err());
    let changed = original.clone();
    let mut handoff = (**changed.state().protocol.task_handoff.as_ref().unwrap()).clone();
    handoff.plans.clear();
    // A changed body cannot borrow the unchanged membership proof.
    let mut tampered_state = changed.state().clone();
    tampered_state.protocol.task_handoff = Some(std::sync::Arc::new(handoff));
    assert!(
        crate::persistence::encode_shared_recovery_state(
            &tampered_state,
            changed.validator_set(),
            changed.validator_registry(),
            changed.retained_validator_sets(),
        )
        .is_err()
    );
    let checkpoint = StateRecoveryCheckpoint::from_persisted(1, &snapshot).unwrap();
    let certified = CertifiedStateRecoveryCheckpoint::new(
        checkpoint.clone(),
        votes(&checkpoint.finality_statement()),
        &snapshot.validator_set,
    )
    .unwrap();
    let (recovered, _recovered_base) = temp_store();
    recovered
        .install_recovered_state(&payload, &certified, &snapshot.validator_set)
        .unwrap();
    let restored = recovered.load().unwrap().unwrap();
    assert!(restored.prepared_tasks.is_empty());
    assert!(!restored.validator_safety_ready);
    let mut state = restored.state;
    let mut book = PreparedTaskBook::new(recovered.clone()).unwrap();
    let before = recovered.load().unwrap().unwrap().generation;
    assert_eq!(
        book.prepare(&mut state, &old, 1, &snapshot.validator_set),
        Err(PreparationError::TaskOriginMismatch {
            expected: 1,
            actual: 2
        })
    );
    assert_eq!(recovered.load().unwrap().unwrap().generation, before);
    let rival = task("handoff-rival", account);
    assert_eq!(
        book.prepare(&mut state, &rival, 1, &snapshot.validator_set),
        Err(PreparationError::CertifiedResourceFence {
            blockers: vec![old.task_id()]
        })
    );
    let independent = task("handoff-independent", crate::test_helpers::account(154));
    book.prepare(&mut state, &independent, 1, &snapshot.validator_set)
        .unwrap();
    assert!(
        !recovered
            .load()
            .unwrap()
            .unwrap()
            .prepared_tasks
            .contains_key(&old.task_id())
    );
    let generation = recovered.load().unwrap().unwrap().generation;
    let wrong = crate::task_abort::statement(
        &old.task_id(),
        old.request_digest(),
        &snapshot.validator_set,
    );
    assert!(
        recovered
            .install_prepared_abort(
                &old.task_id(),
                &FinalityCertificate::new(wrong, votes(&wrong), &snapshot.validator_set).unwrap()
            )
            .is_err()
    );
    assert_eq!(recovered.load().unwrap().unwrap().generation, generation);
    let abort = recovered.prepared_abort_statement(&old.task_id()).unwrap();
    assert_eq!(abort.validator_set_version(), 1);
    let certificate = FinalityCertificate::new(abort, votes(&abort), &validators).unwrap();
    let authorizers = AuthorizerSet::new(
        CURRENT_PROTOCOL_VERSION,
        [key(9).verifying_key().to_bytes()],
    )
    .unwrap();
    let runtime = crate::NodeRuntime::bind_loaded(
        "127.0.0.1:0".parse().unwrap(),
        &recovered,
        recovered.load().unwrap().unwrap(),
        crate::NodeRuntimeCapabilities::default().with_validator(
            crate::ValidatorRuntimeKeys::new(ValidatorId::new(1), key(3), key(4)),
            crate::ValidatorRuntimeConfig::new(
                authorizers,
                crate::BftTimeoutConfig::new(
                    std::time::Duration::from_secs(2),
                    std::time::Duration::from_secs(2),
                    std::time::Duration::from_secs(2),
                ),
                || 1,
            ),
        ),
    )
    .unwrap();
    let forwarded = runtime
        .process_prepared_task_sync(vec![crate::runtime_bft::InboundBftMessage {
            validator_id: ValidatorId::new(2),
            message: crate::BftNetworkMessage::FinalityCertificate {
                scope: crate::ConsensusScope::PreparedTask(old.task_id()),
                certificate: certificate.clone(),
            },
        }])
        .unwrap();
    assert!(forwarded.is_empty());
    let cancelled = recovered.load().unwrap().unwrap();
    assert!(cancelled.state.task_cancelled(old.task_id()));
    assert!(!cancelled.validator_safety_ready);
    assert!(cancelled.validator_vote_locks.is_empty());
    assert!(cancelled.bft_local_states.is_empty());
    book.abort_certified(&mut state, old.task_id(), &certificate)
        .unwrap();
    assert!(state.task_cancelled(old.task_id()));
    book.prepare(&mut state, &rival, 1, &snapshot.validator_set)
        .unwrap();
}
