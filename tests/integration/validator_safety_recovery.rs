use second::{
    CURRENT_PROTOCOL_VERSION, CertifiedStateRecoveryCheckpoint, CertifiedValidatorSetTransition,
    PersistenceError, PublicCurrencyCheckpoint, SecondState, StateRecoveryCheckpoint,
    StateRecoveryPayload, StateStore, ValidatorConsensusKeyRotationRequest, ValidatorCredential,
    ValidatorId, ValidatorRotationAuthority, ValidatorSet, ValidatorSetTransition, ValidatorSigner,
    ValidatorSigningError,
};

use crate::support::{key, signed_vote, temp_base, validator_credential};

fn rotated_set(current: &ValidatorSet, new_consensus_key: [u8; 32]) -> ValidatorSet {
    let mut credentials = (1..=4).map(validator_credential).collect::<Vec<_>>();
    let current_one = current.validator(ValidatorId::new(1)).unwrap();
    credentials[0] = ValidatorCredential::new(
        ValidatorId::new(1),
        current_one.identity_public_key(),
        new_consensus_key,
        current_one.recovery_public_key(),
    )
    .unwrap();
    ValidatorSet::new(current.version() + 1, credentials).unwrap()
}

fn certified_transition(
    current: &ValidatorSet,
    registry: &second::ValidatorRegistry,
    next: &ValidatorSet,
    voters: [u64; 3],
) -> CertifiedValidatorSetTransition {
    let rotation = ValidatorConsensusKeyRotationRequest::sign(
        CURRENT_PROTOCOL_VERSION,
        ValidatorRotationAuthority::Identity,
        ValidatorId::new(1),
        current.version(),
        next.validator(ValidatorId::new(1))
            .unwrap()
            .consensus_public_key(),
        &key(3),
    )
    .unwrap();
    let transition = ValidatorSetTransition::new(
        CURRENT_PROTOCOL_VERSION,
        current,
        registry,
        next.clone(),
        vec![],
        vec![rotation],
    )
    .unwrap();
    let statement = transition.finality_statement();
    let votes = voters
        .into_iter()
        .map(|id| signed_vote(&statement, ValidatorId::new(id), &key((id * 3 + 1) as u8)))
        .collect();
    CertifiedValidatorSetTransition::new(transition, votes, current).unwrap()
}

fn certified_recovery(
    store: &StateStore,
    validators: &ValidatorSet,
    serial: u64,
) -> CertifiedStateRecoveryCheckpoint {
    let persisted = store.load().unwrap().unwrap();
    let checkpoint = StateRecoveryCheckpoint::from_persisted(serial, &persisted).unwrap();
    let statement = checkpoint.finality_statement();
    let votes = [2_u64, 3, 4]
        .into_iter()
        .map(|id| signed_vote(&statement, ValidatorId::new(id), &key((id * 3 + 1) as u8)))
        .collect();
    CertifiedStateRecoveryCheckpoint::new(checkpoint, votes, validators).unwrap()
}

#[test]
fn recovered_validator_reenters_only_after_independent_key_rotation_quorum_and_signing_fence() {
    let validators_v4 = ValidatorSet::new(4, (1..=4).map(validator_credential)).unwrap();
    let source_base = temp_base("validator-safety-source");
    let source_store = StateStore::new(&source_base);
    let state = SecondState::genesis([], 1).with_reserve(1).unwrap();
    source_store.initialize(&state, &validators_v4).unwrap();

    let registry_v4 = source_store.load().unwrap().unwrap().validator_registry;
    let new_consensus_key = key(90);
    let validators_v5 = rotated_set(&validators_v4, new_consensus_key.verifying_key().to_bytes());

    let self_voted_transition =
        certified_transition(&validators_v4, &registry_v4, &validators_v5, [1, 2, 3]);
    let independent_transition =
        certified_transition(&validators_v4, &registry_v4, &validators_v5, [2, 3, 4]);

    source_store
        .activate_validator_set_transition(&independent_transition)
        .unwrap();
    let recovery = certified_recovery(&source_store, &validators_v5, 1);
    let payload =
        StateRecoveryPayload::from_persisted(&source_store.load().unwrap().unwrap()).unwrap();

    let destination_base = temp_base("validator-safety-destination");
    let destination_store = StateStore::new(&destination_base);
    destination_store
        .install_recovered_state(&payload, &recovery, &validators_v5)
        .unwrap();

    let recovered_signer = ValidatorSigner::new(
        ValidatorId::new(1),
        new_consensus_key,
        destination_store.clone(),
    );

    assert_eq!(
        recovered_signer.complete_safety_recovery(&validators_v4, &self_voted_transition,),
        Err(ValidatorSigningError::Persistence(
            PersistenceError::RecoveringValidatorVotedSafetyFenceTransition(ValidatorId::new(1))
        ))
    );
    assert!(
        !destination_store
            .load()
            .unwrap()
            .unwrap()
            .validator_safety_ready
    );

    recovered_signer
        .complete_safety_recovery(&validators_v4, &independent_transition)
        .unwrap();

    let restarted_store = StateStore::new(&destination_base);
    let recovered = restarted_store.load().unwrap().unwrap();
    assert!(recovered.validator_safety_ready);
    assert_eq!(recovered.minimum_signing_validator_set_version, 5);

    let current_checkpoint =
        PublicCurrencyCheckpoint::new(CURRENT_PROTOCOL_VERSION, 1, state.public_currency_summary());
    ValidatorSigner::new(ValidatorId::new(1), key(90), restarted_store.clone())
        .sign_public_checkpoint(&current_checkpoint, &validators_v5)
        .unwrap();

    let old_signer = ValidatorSigner::new(ValidatorId::new(1), key(4), restarted_store.clone());
    let old_checkpoint =
        PublicCurrencyCheckpoint::new(CURRENT_PROTOCOL_VERSION, 1, state.public_currency_summary());
    assert!(matches!(
        old_signer.sign_public_checkpoint(&old_checkpoint, &validators_v4),
        Err(ValidatorSigningError::Persistence(
            PersistenceError::SigningFenceViolation {
                minimum_validator_set_version: 5,
                actual_validator_set_version: 4,
            }
        ))
    ));

    destination_store.remove_files().unwrap();
    source_store.remove_files().unwrap();
}
