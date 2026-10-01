use second::{
    BftPhase, BftValue, CURRENT_PROTOCOL_VERSION, ConsensusScope, PersistenceError,
    PublicCurrencyCheckpoint, SecondState, StateStore, ValidatorId, ValidatorSigner,
    ValidatorSigningError,
};

use crate::support::{bft_qc, key, temp_base, validator_set};

#[test]
fn prevote_qc_enables_precommit_and_lock_survives_restart() {
    let validators = validator_set(7, 1..=4);
    let base = temp_base("bft-lock-restart");
    let store = StateStore::new(&base);
    let state = second::SecondState::genesis([], 1);
    store.initialize(&state, &validators).unwrap();

    let scope = ConsensusScope::PublicCheckpoint {
        validator_set_version: 7,
        epoch: 1,
    };
    let digest = [7; 32];
    let signer = ValidatorSigner::new(ValidatorId::new(1), key(4), store.clone());

    signer
        .sign_bft_prevote(
            scope.clone(),
            0,
            BftValue::Digest(digest),
            &validators,
            None,
        )
        .unwrap();

    let prevote_qc = bft_qc(
        scope.clone(),
        0,
        BftPhase::Prevote,
        BftValue::Digest(digest),
        [1, 2, 3],
        &validators,
    );
    signer
        .sign_bft_precommit(
            scope.clone(),
            0,
            BftValue::Digest(digest),
            &validators,
            Some(&prevote_qc),
        )
        .unwrap();

    let restarted = StateStore::new(&base);
    let persisted = restarted
        .bft_local_state(ValidatorId::new(1), &scope)
        .unwrap()
        .unwrap();
    assert_eq!(persisted.round(), 0);
    assert_eq!(persisted.locked_round(), Some(0));
    assert_eq!(persisted.locked_digest(), Some(digest));

    let precommit_qc = bft_qc(
        scope.clone(),
        0,
        BftPhase::Precommit,
        BftValue::Digest(digest),
        [1, 2, 3],
        &validators,
    );
    restarted
        .accept_bft_precommit_qc(ValidatorId::new(1), &precommit_qc, &validators)
        .unwrap();
    assert!(
        restarted
            .bft_finality_ready(ValidatorId::new(1), &scope, digest)
            .unwrap()
    );

    restarted.remove_files().unwrap();
}

#[test]
fn irreversible_finality_vote_requires_precommit_qc() {
    let validators = validator_set(7, 1..=4);
    let store = StateStore::new(temp_base("bft-finality-gate"));
    let state = SecondState::genesis([], 1).with_reserve(1).unwrap();
    store.initialize(&state, &validators).unwrap();

    let checkpoint = PublicCurrencyCheckpoint::new(
        CURRENT_PROTOCOL_VERSION,
        10,
        state.public_currency_summary(),
    );
    let scope = ConsensusScope::PublicCheckpoint {
        validator_set_version: validators.version(),
        epoch: checkpoint.epoch(),
    };
    let digest = checkpoint
        .finality_statement(validators.version())
        .subject_digest();
    let signer = ValidatorSigner::new(ValidatorId::new(1), key(4), store.clone());

    assert_eq!(
        signer.sign_public_checkpoint(&checkpoint, &validators),
        Err(ValidatorSigningError::Persistence(
            PersistenceError::BftFinalityNotReady
        ))
    );

    let precommit_qc = bft_qc(
        scope.clone(),
        0,
        BftPhase::Precommit,
        BftValue::Digest(digest),
        [1, 2, 3],
        &validators,
    );
    store
        .accept_bft_precommit_qc(ValidatorId::new(1), &precommit_qc, &validators)
        .unwrap();

    signer
        .sign_public_checkpoint(&checkpoint, &validators)
        .unwrap();
    assert!(
        store
            .bft_local_state(ValidatorId::new(1), &scope)
            .unwrap()
            .is_none()
    );

    store.remove_files().unwrap();
}

#[test]
fn lock_rejects_conflicting_prevote_until_higher_round_qc_unlocks_it() {
    let validators = validator_set(7, 1..=4);
    let store = StateStore::new(temp_base("bft-lock-migration"));
    let state = second::SecondState::genesis([], 1);
    store.initialize(&state, &validators).unwrap();

    let scope = ConsensusScope::PublicCheckpoint {
        validator_set_version: 7,
        epoch: 2,
    };
    let a = [1; 32];
    let b = [2; 32];
    let signer = ValidatorSigner::new(ValidatorId::new(1), key(4), store.clone());

    signer
        .sign_bft_prevote(scope.clone(), 0, BftValue::Digest(a), &validators, None)
        .unwrap();
    let a_qc = bft_qc(
        scope.clone(),
        0,
        BftPhase::Prevote,
        BftValue::Digest(a),
        [1, 2, 3],
        &validators,
    );
    signer
        .sign_bft_precommit(
            scope.clone(),
            0,
            BftValue::Digest(a),
            &validators,
            Some(&a_qc),
        )
        .unwrap();

    store
        .advance_bft_round(ValidatorId::new(1), &scope, 1)
        .unwrap();
    assert!(
        signer
            .sign_bft_prevote(scope.clone(), 1, BftValue::Digest(b), &validators, None)
            .is_err()
    );

    signer
        .sign_bft_prevote(scope.clone(), 1, BftValue::Nil, &validators, None)
        .unwrap();
    store
        .advance_bft_round(ValidatorId::new(1), &scope, 2)
        .unwrap();

    let b_round1_qc = bft_qc(
        scope.clone(),
        1,
        BftPhase::Prevote,
        BftValue::Digest(b),
        [2, 3, 4],
        &validators,
    );
    signer
        .sign_bft_prevote(
            scope.clone(),
            2,
            BftValue::Digest(b),
            &validators,
            Some(&b_round1_qc),
        )
        .unwrap();

    let b_round2_qc = bft_qc(
        scope.clone(),
        2,
        BftPhase::Prevote,
        BftValue::Digest(b),
        [1, 2, 3],
        &validators,
    );
    signer
        .sign_bft_precommit(
            scope.clone(),
            2,
            BftValue::Digest(b),
            &validators,
            Some(&b_round2_qc),
        )
        .unwrap();

    let persisted = store
        .bft_local_state(ValidatorId::new(1), &scope)
        .unwrap()
        .unwrap();
    assert_eq!(persisted.locked_round(), Some(2));
    assert_eq!(persisted.locked_digest(), Some(b));

    store.remove_files().unwrap();
}
