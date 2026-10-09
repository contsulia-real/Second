use super::*;
use crate::*;
use ed25519_dalek::SigningKey;
use std::time::Duration;

fn fixture() -> (
    ValidatorId,
    ValidatorSet,
    std::path::PathBuf,
    StateStore,
    NodeRuntime,
) {
    let key = |seed| SigningKey::from_bytes(&[seed; 32]);
    let id = ValidatorId::new(1);
    let validators = ValidatorSet::new(
        1,
        [ValidatorCredential::new(
            id,
            key(1).verifying_key().to_bytes(),
            key(2).verifying_key().to_bytes(),
            key(3).verifying_key().to_bytes(),
        )
        .unwrap()],
    )
    .unwrap();
    let (store, base) = crate::prepared::tests::temp_store();
    store
        .initialize(&SecondState::genesis([], 1), &validators)
        .unwrap();
    let runtime = NodeRuntime::bind_loaded(
        "127.0.0.1:0".parse().unwrap(),
        &store,
        store.load().unwrap().unwrap(),
        NodeRuntimeCapabilities::default().with_validator(
            ValidatorRuntimeKeys::new(id, key(1), key(2)),
            ValidatorRuntimeConfig::new(
                AuthorizerSet::new(
                    CURRENT_PROTOCOL_VERSION,
                    [key(9).verifying_key().to_bytes()],
                )
                .unwrap(),
                BftTimeoutConfig::new(
                    Duration::from_secs(2),
                    Duration::from_secs(2),
                    Duration::from_secs(2),
                ),
                || 1,
            ),
        ),
    )
    .unwrap();
    (id, validators, base, store, runtime)
}

#[tokio::test]
async fn uncertified_sources_cannot_jump_epochs_but_verified_proofs_can_catch_up() {
    let key = |seed| SigningKey::from_bytes(&[seed; 32]);
    let (id, validators, _base, store, runtime) = fixture();
    let context = runtime.governance_context().unwrap();
    let summary = store
        .load()
        .unwrap()
        .unwrap()
        .state
        .public_currency_summary();
    let candidate =
        PublicCurrencyCheckpoint::new(CURRENT_PROTOCOL_VERSION, u64::MAX - 1, summary.clone());
    let scope = ConsensusScope::PublicCheckpoint {
        validator_set_version: 1,
        epoch: candidate.epoch(),
    };
    assert_eq!(
        install_public_checkpoint_source(&context, 1, &scope, &candidate.encode_source(1)),
        Err(BftConsensusRuntimeError::InvalidGovernanceSource)
    );
    assert_eq!(store.load().unwrap().unwrap().checkpoint_floor_epoch, 0);
    let checkpoint = PublicCurrencyCheckpoint::new(CURRENT_PROTOCOL_VERSION, 99, summary);
    let statement = checkpoint.finality_statement(1);
    let scope = ConsensusScope::PublicCheckpoint {
        validator_set_version: 1,
        epoch: 99,
    };
    let forged = PublicCurrencyCheckpointProof::new(
        checkpoint.clone(),
        1,
        vec![ValidatorVote::sign_unchecked(&statement, id, &key(4))],
    );
    assert!(
        install_public_checkpoint_source(&context, 1, &scope, &forged.encode_bytes().unwrap())
            .is_err()
    );
    assert_eq!(store.load().unwrap().unwrap().checkpoint_floor_epoch, 0);
    let valid = PublicCurrencyCheckpointProof::new(
        checkpoint.clone(),
        1,
        vec![ValidatorVote::sign_unchecked(&statement, id, &key(2))],
    );
    install_public_checkpoint_source(&context, 1, &scope, &valid.encode_bytes().unwrap()).unwrap();
    let snapshot = store.load().unwrap().unwrap();
    assert_eq!(snapshot.checkpoint_floor_epoch, 99);
    assert_eq!(
        snapshot.public_checkpoint_proof.unwrap().checkpoint(),
        &checkpoint
    );
    let ahead = PublicCurrencyCheckpoint::new(
        CURRENT_PROTOCOL_VERSION,
        100,
        SecondState::genesis([], 1)
            .with_reserve(1)
            .unwrap()
            .public_currency_summary(),
    );
    let statement = ahead.finality_statement(1);
    let proof = PublicCurrencyCheckpointProof::new(
        ahead,
        1,
        vec![ValidatorVote::sign_unchecked(&statement, id, &key(2))],
    );
    let scope = ConsensusScope::PublicCheckpoint {
        validator_set_version: 1,
        epoch: 100,
    };
    install_public_checkpoint_source(&context, 1, &scope, &proof.encode_bytes().unwrap()).unwrap();
    assert_eq!(store.load().unwrap().unwrap().checkpoint_floor_epoch, 100);
    assert_eq!(
        store.next_public_currency_checkpoint().unwrap().epoch(),
        101
    );
    assert!(matches!(
        store.attach_certified_checkpoint(&valid.verify_checkpoint(&validators).unwrap()),
        Err(PersistenceError::StaleCheckpointEpoch { .. })
    ));
    drop(context);
    drop(runtime);
    store.remove_files().unwrap();
}

#[tokio::test]
async fn only_quorum_recovery_evidence_can_advance_a_stale_state_and_schedule_its_successor() {
    let key = |seed| SigningKey::from_bytes(&[seed; 32]);
    let (id, validators, _base, store, runtime) = fixture();
    let original = store.load().unwrap().unwrap();
    let checkpoint = StateRecoveryCheckpoint::from_persisted(5, &original).unwrap();
    let advanced = original.state.clone().with_reserve(1).unwrap();
    store
        .save_with_prepared(
            &original.state,
            &advanced,
            &validators,
            &std::collections::BTreeMap::new(),
            &std::collections::BTreeMap::new(),
        )
        .unwrap();
    let context = runtime.governance_context().unwrap();
    let scope = ConsensusScope::StateRecoveryCheckpoint {
        validator_set_version: 1,
        serial: 5,
    };
    let generation = store.load().unwrap().unwrap().generation;
    assert!(
        install_recovery_source(&context, 1, &scope, &checkpoint.encode_source().unwrap()).is_err()
    );
    let forged = StateRecoveryCheckpointProof::new(
        checkpoint.clone(),
        vec![ValidatorVote::sign_unchecked(
            &checkpoint.finality_statement(),
            id,
            &key(4),
        )],
    );
    assert!(install_recovery_source(&context, 1, &scope, &forged.encode_bytes().unwrap()).is_err());
    assert_eq!(store.load().unwrap().unwrap().generation, generation);
    let valid = StateRecoveryCheckpointProof::new(
        checkpoint.clone(),
        vec![ValidatorVote::sign_unchecked(
            &checkpoint.finality_statement(),
            id,
            &key(2),
        )],
    );
    let certified = valid.clone().verify_checkpoint(&validators).unwrap();
    assert_eq!(
        store.advance_recovery_checkpoint_floor(&certified),
        Err(PersistenceError::RecoveryCheckpointDoesNotMatchState)
    );
    let bytes = valid.encode_bytes().unwrap();
    install_recovery_source(&context, 1, &scope, &bytes).unwrap();
    let snapshot = StateStore::new(&_base).load().unwrap().unwrap();
    let floor = snapshot.recovery_checkpoint_floors.get(&1).unwrap();
    assert!(floor.certified);
    assert_eq!(floor.serial, 5);
    assert!(snapshot.recovery_checkpoint_proof.is_none());
    let successor = store.next_state_recovery_checkpoint().unwrap();
    assert_eq!(successor.serial(), 6);
    assert!(successor.matches_persisted(&snapshot).unwrap());
    assert!(successor.was_admitted(&snapshot));
    let generation = snapshot.generation;
    install_recovery_source(&context, 1, &scope, &bytes).unwrap();
    assert_eq!(store.load().unwrap().unwrap().generation, generation);
    drop(context);
    drop(runtime);
    store.remove_files().unwrap();
}
