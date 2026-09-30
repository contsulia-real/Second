use crate::support;

use second::{
    CURRENT_PROTOCOL_VERSION, SecondState, StateStore, ValidatorConsensusKeyRotationRequest,
    ValidatorCredential, ValidatorId, ValidatorRegistry, ValidatorRegistryError,
    ValidatorRotationAuthority, ValidatorSet, ValidatorSetTransition, ValidatorStatus,
};
use support::{
    certify_and_activate_validator_transition, key, temp_base, validator_credential as credential,
    validator_set as set,
};

#[test]
fn retired_validator_id_can_never_rejoin() {
    let current = set(1, 1..=4);
    let mut registry = ValidatorRegistry::from_validator_set(&current).unwrap();
    let without_four = set(2, 1..=3);

    let transition = ValidatorSetTransition::new(
        CURRENT_PROTOCOL_VERSION,
        &current,
        &registry,
        without_four.clone(),
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    certify_and_activate_validator_transition(&current, &mut registry, transition, [1, 2, 3]);
    assert_eq!(
        registry.status(ValidatorId::new(4)),
        Some(ValidatorStatus::Retired)
    );

    let rejoined = set(3, 1..=4);
    assert_eq!(
        registry.validate_transition(&without_four, &rejoined),
        Err(ValidatorRegistryError::ValidatorIdAlreadyUsed(
            ValidatorId::new(4)
        ))
    );
}

#[test]
fn historical_consensus_key_can_never_be_reassigned() {
    let current = set(1, 1..=4);
    let old_consensus = current
        .validator(ValidatorId::new(1))
        .unwrap()
        .consensus_public_key();
    let mut registry = ValidatorRegistry::from_validator_set(&current).unwrap();

    let rotated_one = ValidatorCredential::new(
        ValidatorId::new(1),
        current
            .validator(ValidatorId::new(1))
            .unwrap()
            .identity_public_key(),
        key(100).verifying_key().to_bytes(),
        current
            .validator(ValidatorId::new(1))
            .unwrap()
            .recovery_public_key(),
    )
    .unwrap();
    let rotated = ValidatorSet::new(
        2,
        std::iter::once(rotated_one).chain((2..=4).map(credential)),
    )
    .unwrap();
    let rotation = ValidatorConsensusKeyRotationRequest::sign(
        CURRENT_PROTOCOL_VERSION,
        ValidatorRotationAuthority::Identity,
        ValidatorId::new(1),
        current.version(),
        key(100).verifying_key().to_bytes(),
        &key(3),
    )
    .unwrap();
    let transition = ValidatorSetTransition::new(
        CURRENT_PROTOCOL_VERSION,
        &current,
        &registry,
        rotated.clone(),
        Vec::new(),
        vec![rotation],
    )
    .unwrap();
    certify_and_activate_validator_transition(&current, &mut registry, transition, [1, 2, 3]);

    let reused = ValidatorCredential::new(
        ValidatorId::new(5),
        key(101).verifying_key().to_bytes(),
        old_consensus,
        key(102).verifying_key().to_bytes(),
    )
    .unwrap();
    let candidate = ValidatorSet::new(
        3,
        (1..=4)
            .map(|id| rotated.validator(ValidatorId::new(id)).unwrap().clone())
            .chain(std::iter::once(reused)),
    )
    .unwrap();

    assert_eq!(
        registry.validate_transition(&rotated, &candidate),
        Err(ValidatorRegistryError::ValidatorKeyAlreadyUsed(
            ValidatorId::new(5)
        ))
    );
}

#[test]
fn retired_identity_history_survives_snapshot_restart() {
    let current = set(1, 1..=4);
    let without_four = set(2, 1..=3);
    let mut registry = ValidatorRegistry::from_validator_set(&current).unwrap();
    let transition = ValidatorSetTransition::new(
        CURRENT_PROTOCOL_VERSION,
        &current,
        &registry,
        without_four.clone(),
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    certify_and_activate_validator_transition(&current, &mut registry, transition, [1, 2, 3]);

    let store = StateStore::new(temp_base("validator-registry"));
    let state = SecondState::genesis([], 1);
    store
        .initialize_with_validator_registry(&state, &without_four, &registry)
        .unwrap();

    let restored = store.load().unwrap().unwrap();
    assert_eq!(
        restored.validator_registry.status(ValidatorId::new(4)),
        Some(ValidatorStatus::Retired)
    );
    assert_eq!(
        restored.validator_registry.active_validator_set_version(),
        without_four.version()
    );

    let rejoined = set(3, 1..=4);
    assert_eq!(
        restored
            .validator_registry
            .validate_transition(&restored.validator_set, &rejoined),
        Err(ValidatorRegistryError::ValidatorIdAlreadyUsed(
            ValidatorId::new(4)
        ))
    );

    store.remove_files().unwrap();
}
