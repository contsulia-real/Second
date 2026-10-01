use second::{
    BftDriver, BftDriverAction, BftDriverError, BftDriverPhase, BftPhase, BftStatement, BftValue,
    CURRENT_PROTOCOL_VERSION, PublicCurrencyCheckpoint, SecondState, StateStore, ValidatorId,
    ValidatorSigner,
};

use crate::support::{bft_qc, key, signed_bft_vote, temp_base, validator_set};

#[test]
fn timeout_view_change_is_durable_and_rotates_proposer() {
    let validators = validator_set(7, 1..=4);
    let base = temp_base("bft-driver-timeout-view-change");
    let store = StateStore::new(&base);
    let state = SecondState::genesis([], 1).with_reserve(1).unwrap();
    store.initialize(&state, &validators).unwrap();

    let checkpoint =
        PublicCurrencyCheckpoint::new(CURRENT_PROTOCOL_VERSION, 1, state.public_currency_summary());
    let subject = store
        .public_checkpoint_bft_proposal_subject(&checkpoint)
        .unwrap();
    let scope = subject.scope().clone();
    let signer = ValidatorSigner::new(ValidatorId::new(1), key(4), store.clone());
    let mut driver =
        BftDriver::new(signer, store.clone(), validators.clone(), scope.clone()).unwrap();

    assert_eq!(driver.phase().unwrap(), BftDriverPhase::Proposal);
    assert_eq!(driver.proposer().unwrap(), ValidatorId::new(1));

    match driver.on_timeout().unwrap() {
        BftDriverAction::Vote {
            statement, vote, ..
        } => {
            assert_eq!(statement.phase(), BftPhase::Prevote);
            assert_eq!(statement.value(), BftValue::Nil);
            assert_eq!(vote.validator_id(), ValidatorId::new(1));
        }
        action => panic!("unexpected proposal timeout action: {action:?}"),
    }
    assert_eq!(driver.phase().unwrap(), BftDriverPhase::Prevote);

    match driver.on_timeout().unwrap() {
        BftDriverAction::Vote {
            statement, vote, ..
        } => {
            assert_eq!(statement.phase(), BftPhase::Precommit);
            assert_eq!(statement.value(), BftValue::Nil);
            assert_eq!(vote.validator_id(), ValidatorId::new(1));
        }
        action => panic!("unexpected prevote timeout action: {action:?}"),
    }
    assert_eq!(driver.phase().unwrap(), BftDriverPhase::Precommit);

    assert_eq!(
        driver.on_timeout().unwrap(),
        BftDriverAction::RoundAdvanced {
            round: 1,
            proposer: ValidatorId::new(2),
        }
    );

    let restarted_store = StateStore::new(&base);
    let restarted = BftDriver::new(
        ValidatorSigner::new(ValidatorId::new(1), key(4), restarted_store.clone()),
        restarted_store.clone(),
        validators,
        scope,
    )
    .unwrap();
    assert_eq!(restarted.current_round().unwrap(), 1);
    assert_eq!(restarted.phase().unwrap(), BftDriverPhase::Proposal);
    assert_eq!(restarted.proposer().unwrap(), ValidatorId::new(2));

    restarted_store.remove_files().unwrap();
}

#[test]
fn digest_qc_cannot_reach_signer_without_locally_validated_subject() {
    let validators = validator_set(7, 1..=4);
    let store = StateStore::new(temp_base("bft-driver-subject-gate"));
    let state = SecondState::genesis([], 1).with_reserve(1).unwrap();
    store.initialize(&state, &validators).unwrap();

    let checkpoint =
        PublicCurrencyCheckpoint::new(CURRENT_PROTOCOL_VERSION, 2, state.public_currency_summary());
    let subject = store
        .public_checkpoint_bft_proposal_subject(&checkpoint)
        .unwrap();
    let signer = ValidatorSigner::new(ValidatorId::new(1), key(4), store.clone());
    let mut driver = BftDriver::new(
        signer,
        store.clone(),
        validators.clone(),
        subject.scope().clone(),
    )
    .unwrap();
    let certificate = bft_qc(
        subject.scope().clone(),
        0,
        BftPhase::Prevote,
        BftValue::Digest(subject.digest()),
        [1, 2, 3],
        &validators,
    );

    assert_eq!(
        driver.accept_quorum_certificate(&certificate),
        Err(BftDriverError::SubjectMismatch)
    );
    assert!(
        store
            .bft_local_state(ValidatorId::new(1), subject.scope())
            .unwrap()
            .is_none()
    );

    store.remove_files().unwrap();
}

#[test]
fn nil_precommit_qc_advances_a_validator_that_missed_the_round() {
    let validators = validator_set(7, 1..=4);
    let store = StateStore::new(temp_base("bft-driver-missed-round"));
    let state = SecondState::genesis([], 1).with_reserve(1).unwrap();
    store.initialize(&state, &validators).unwrap();

    let checkpoint =
        PublicCurrencyCheckpoint::new(CURRENT_PROTOCOL_VERSION, 3, state.public_currency_summary());
    let subject = store
        .public_checkpoint_bft_proposal_subject(&checkpoint)
        .unwrap();
    let scope = subject.scope().clone();
    let mut driver = BftDriver::new(
        ValidatorSigner::new(ValidatorId::new(1), key(4), store.clone()),
        store.clone(),
        validators.clone(),
        scope.clone(),
    )
    .unwrap();

    assert!(
        store
            .bft_local_state(ValidatorId::new(1), &scope)
            .unwrap()
            .is_none()
    );

    let nil_qc = bft_qc(
        scope.clone(),
        0,
        BftPhase::Precommit,
        BftValue::Nil,
        [1, 2, 3],
        &validators,
    );
    assert_eq!(
        driver.accept_quorum_certificate(&nil_qc).unwrap(),
        BftDriverAction::RoundAdvanced {
            round: 1,
            proposer: ValidatorId::new(2),
        }
    );
    assert_eq!(driver.current_round().unwrap(), 1);
    assert_eq!(driver.phase().unwrap(), BftDriverPhase::Proposal);

    store.remove_files().unwrap();
}

#[test]
fn late_digest_precommit_qc_finalizes_after_local_round_advance() {
    let validators = validator_set(7, 1..=4);
    let store = StateStore::new(temp_base("bft-driver-late-finality"));
    let state = SecondState::genesis([], 1).with_reserve(1).unwrap();
    store.initialize(&state, &validators).unwrap();

    let checkpoint =
        PublicCurrencyCheckpoint::new(CURRENT_PROTOCOL_VERSION, 3, state.public_currency_summary());
    let subject = store
        .public_checkpoint_bft_proposal_subject(&checkpoint)
        .unwrap();
    let scope = subject.scope().clone();
    let digest = subject.digest();
    let mut driver = BftDriver::new(
        ValidatorSigner::new(ValidatorId::new(1), key(4), store.clone()),
        store.clone(),
        validators.clone(),
        scope.clone(),
    )
    .unwrap();
    driver.register_subject(&subject).unwrap();

    assert!(matches!(
        driver.on_timeout().unwrap(),
        BftDriverAction::Vote { .. }
    ));
    assert!(matches!(
        driver.on_timeout().unwrap(),
        BftDriverAction::Vote { .. }
    ));
    assert_eq!(
        driver.on_timeout().unwrap(),
        BftDriverAction::RoundAdvanced {
            round: 1,
            proposer: ValidatorId::new(2),
        }
    );
    assert_eq!(driver.current_round().unwrap(), 1);

    let late_finality_qc = bft_qc(
        scope.clone(),
        0,
        BftPhase::Precommit,
        BftValue::Digest(digest),
        [1, 2, 3],
        &validators,
    );
    assert_eq!(
        driver.accept_quorum_certificate(&late_finality_qc).unwrap(),
        BftDriverAction::FinalityReady { round: 0, digest }
    );
    assert_eq!(
        driver.current_round().unwrap(),
        1,
        "accepting an older finality QC must not move the local round backwards"
    );
    assert!(
        store
            .bft_finality_ready(ValidatorId::new(1), &scope, digest)
            .unwrap()
    );

    store.remove_files().unwrap();
}

#[test]
fn late_prevote_qc_does_not_double_sign_after_nil_precommit() {
    let validators = validator_set(7, 1..=4);
    let store = StateStore::new(temp_base("bft-driver-late-prevote-qc"));
    let state = SecondState::genesis([], 1).with_reserve(1).unwrap();
    store.initialize(&state, &validators).unwrap();

    let checkpoint =
        PublicCurrencyCheckpoint::new(CURRENT_PROTOCOL_VERSION, 3, state.public_currency_summary());
    let subject = store
        .public_checkpoint_bft_proposal_subject(&checkpoint)
        .unwrap();
    let scope = subject.scope().clone();
    let digest = subject.digest();
    let mut driver = BftDriver::new(
        ValidatorSigner::new(ValidatorId::new(1), key(4), store.clone()),
        store.clone(),
        validators.clone(),
        scope.clone(),
    )
    .unwrap();
    driver.register_subject(&subject).unwrap();

    assert!(matches!(
        driver.on_timeout().unwrap(),
        BftDriverAction::Vote { .. }
    ));
    let precommit = driver.on_timeout().unwrap();
    assert!(matches!(
        precommit,
        BftDriverAction::Vote {
            ref statement,
            ..
        } if statement.phase() == BftPhase::Precommit
            && statement.value() == BftValue::Nil
    ));

    let certificate = bft_qc(
        scope.clone(),
        0,
        BftPhase::Prevote,
        BftValue::Digest(digest),
        [1, 2, 3],
        &validators,
    );
    assert_eq!(
        driver.accept_quorum_certificate(&certificate).unwrap(),
        BftDriverAction::Noop
    );
    assert!(matches!(
        driver.on_timeout().unwrap(),
        BftDriverAction::RoundAdvanced { round: 1, .. }
    ));

    store.remove_files().unwrap();
}

#[test]
fn late_digest_precommit_votes_can_still_form_finality_qc() {
    let validators = validator_set(7, 1..=4);
    let store = StateStore::new(temp_base("bft-driver-late-precommit-votes"));
    let state = SecondState::genesis([], 1).with_reserve(1).unwrap();
    store.initialize(&state, &validators).unwrap();

    let checkpoint =
        PublicCurrencyCheckpoint::new(CURRENT_PROTOCOL_VERSION, 3, state.public_currency_summary());
    let subject = store
        .public_checkpoint_bft_proposal_subject(&checkpoint)
        .unwrap();
    let scope = subject.scope().clone();
    let digest = subject.digest();
    let mut driver = BftDriver::new(
        ValidatorSigner::new(ValidatorId::new(1), key(4), store.clone()),
        store.clone(),
        validators.clone(),
        scope.clone(),
    )
    .unwrap();
    driver.register_subject(&subject).unwrap();

    assert!(matches!(
        driver.on_timeout().unwrap(),
        BftDriverAction::Vote { .. }
    ));
    assert!(matches!(
        driver.on_timeout().unwrap(),
        BftDriverAction::Vote { .. }
    ));
    assert!(matches!(
        driver.on_timeout().unwrap(),
        BftDriverAction::RoundAdvanced { round: 1, .. }
    ));

    let statement = BftStatement::new(
        validators.version(),
        scope.clone(),
        0,
        BftPhase::Precommit,
        BftValue::Digest(digest),
    );
    let mut certificate = None;
    for validator_id in [1, 2, 3] {
        if let Some(BftDriverAction::QuorumCertificate(qc)) = driver
            .ingest_vote(
                statement.clone(),
                signed_bft_vote(&statement, ValidatorId::new(validator_id)),
            )
            .unwrap()
        {
            certificate = Some(qc);
        }
    }
    let certificate = certificate.expect("late precommit votes must still form finality QC");
    assert_eq!(
        driver.accept_quorum_certificate(&certificate).unwrap(),
        BftDriverAction::FinalityReady { round: 0, digest }
    );
    assert_eq!(driver.current_round().unwrap(), 1);
    assert!(
        store
            .bft_finality_ready(ValidatorId::new(1), &scope, digest)
            .unwrap()
    );

    store.remove_files().unwrap();
}

#[test]
fn proposal_votes_and_qcs_drive_existing_safety_core_to_finality_ready() {
    let validators = validator_set(7, 1..=4);
    let store = StateStore::new(temp_base("bft-driver-finality"));
    let state = SecondState::genesis([], 1).with_reserve(1).unwrap();
    store.initialize(&state, &validators).unwrap();

    let checkpoint =
        PublicCurrencyCheckpoint::new(CURRENT_PROTOCOL_VERSION, 3, state.public_currency_summary());
    let subject = store
        .public_checkpoint_bft_proposal_subject(&checkpoint)
        .unwrap();
    let scope = subject.scope().clone();
    let digest = subject.digest();
    let signer = ValidatorSigner::new(ValidatorId::new(1), key(4), store.clone());
    let mut driver =
        BftDriver::new(signer, store.clone(), validators.clone(), scope.clone()).unwrap();

    let proposal = driver.create_proposal(&subject).unwrap();
    let (prevote_statement, own_prevote) =
        match driver.accept_proposal(&proposal, &subject, None).unwrap() {
            BftDriverAction::Vote { statement, vote } => (statement, vote),
            action => panic!("unexpected proposal action: {action:?}"),
        };

    let mut prevote_qc = None;
    for vote in [
        own_prevote,
        signed_bft_vote(&prevote_statement, ValidatorId::new(2)),
        signed_bft_vote(&prevote_statement, ValidatorId::new(3)),
    ] {
        if let Some(BftDriverAction::QuorumCertificate(certificate)) =
            driver.ingest_vote(prevote_statement.clone(), vote).unwrap()
        {
            prevote_qc = Some(certificate);
        }
    }
    let prevote_qc = prevote_qc.expect("three valid prevotes must form quorum");
    assert_eq!(
        driver
            .ingest_vote(
                prevote_statement.clone(),
                signed_bft_vote(&prevote_statement, ValidatorId::new(4)),
            )
            .unwrap(),
        None,
        "additional votes after quorum must not emit the same QC again"
    );

    let (precommit_statement, own_precommit) =
        match driver.accept_quorum_certificate(&prevote_qc).unwrap() {
            BftDriverAction::Vote { statement, vote } => (statement, vote),
            action => panic!("unexpected prevote QC action: {action:?}"),
        };

    let mut precommit_qc = None;
    for vote in [
        own_precommit,
        signed_bft_vote(&precommit_statement, ValidatorId::new(2)),
        signed_bft_vote(&precommit_statement, ValidatorId::new(3)),
    ] {
        if let Some(BftDriverAction::QuorumCertificate(certificate)) = driver
            .ingest_vote(precommit_statement.clone(), vote)
            .unwrap()
        {
            precommit_qc = Some(certificate);
        }
    }
    let precommit_qc = precommit_qc.expect("three valid precommits must form quorum");

    assert_eq!(
        driver.accept_quorum_certificate(&precommit_qc).unwrap(),
        BftDriverAction::FinalityReady { round: 0, digest }
    );
    assert!(
        store
            .bft_finality_ready(ValidatorId::new(1), &scope, digest)
            .unwrap()
    );

    store.remove_files().unwrap();
}
