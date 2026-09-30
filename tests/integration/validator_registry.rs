use crate::support;

use second::{
    SecondState, StateStore, ValidatorCredential, ValidatorId, ValidatorRegistry,
    ValidatorRegistryError, ValidatorSet, ValidatorStatus,
};
use support::{key, temp_base, validator_credential as credential, validator_set as set};

#[test]
fn retired_validator_id_can_never_rejoin() {
    let current = set(1, 1..=4);
    let mut registry = ValidatorRegistry::from_validator_set(&current).unwrap();
    let without_four = set(2, 1..=3);

    registry.apply_next_set(&without_four).unwrap();
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
    registry.apply_next_set(&rotated).unwrap();

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
    registry.apply_next_set(&without_four).unwrap();

    let store = StateStore::new(temp_base("validator-registry"));
    let state = SecondState::genesis([], 1);
    store
        .save_with_validator_registry(&state, &without_four, &registry)
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

    let forgotten = ValidatorRegistry::from_validator_set(&restored.validator_set).unwrap();
    assert_eq!(
        store.save_with_validator_registry(&restored.state, &restored.validator_set, &forgotten,),
        Err(second::PersistenceError::ValidatorRegistryMismatch)
    );

    store.remove_files().unwrap();
}
