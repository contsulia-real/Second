use crate::support;

use second::{
    CURRENT_PROTOCOL_VERSION, CertifiedValidatorSetTransition, ExecutionOutcome, Operation,
    PersistenceError, PreparedTaskBook, PublicCurrencyCheckpoint, SecondState, StateStore,
    ValidatorConsensusKeyRotationRequest, ValidatorCredential, ValidatorId, ValidatorRegistry,
    ValidatorRotationAuthority, ValidatorSet, ValidatorSetTransition, ValidatorSigner,
    ValidatorSigningError, ValidatorTransitionError,
};
use support::{
    certificate_from_keys, key, signed_vote, temp_base, validator_credential as credential,
    verified_task,
};

fn current_set() -> ValidatorSet {
    ValidatorSet::new(4, (1..=4).map(credential)).unwrap()
}

fn registry(current: &ValidatorSet) -> ValidatorRegistry {
    ValidatorRegistry::from_validator_set(current).unwrap()
}

fn admission(id: u64) -> second::VerifiedValidatorAdmission {
    second::ValidatorAdmissionRequest::sign(
        CURRENT_PROTOCOL_VERSION,
        credential(id),
        &key((id * 3) as u8),
        &key((id * 3 + 1) as u8),
        &key((id * 3 + 2) as u8),
    )
    .unwrap()
    .verify()
    .unwrap()
}

#[test]
fn validator_credential_requires_three_distinct_keys() {
    let shared = key(7).verifying_key().to_bytes();

    assert!(
        ValidatorCredential::new(
            ValidatorId::new(1),
            shared,
            shared,
            key(8).verifying_key().to_bytes(),
        )
        .is_err()
    );
}

#[test]
fn validator_set_rejects_key_reuse_across_different_validators() {
    let first = credential(1);
    let reused_identity = ValidatorCredential::new(
        ValidatorId::new(2),
        first.identity_public_key(),
        key(20).verifying_key().to_bytes(),
        key(21).verifying_key().to_bytes(),
    )
    .unwrap();

    assert!(ValidatorSet::new(1, [first, reused_identity]).is_err());
}

#[test]
fn current_quorum_can_certify_complete_next_validator_set() {
    let current = current_set();
    let next = ValidatorSet::new(5, (1..=5).map(credential)).unwrap();
    let transition = ValidatorSetTransition::new(
        CURRENT_PROTOCOL_VERSION,
        &current,
        &registry(&current),
        next,
        vec![admission(5)],
        Vec::new(),
    )
    .unwrap();
    let statement = transition.finality_statement();

    let votes = [1_u64, 2, 3]
        .into_iter()
        .map(|id| signed_vote(&statement, ValidatorId::new(id), &key((id * 3 + 1) as u8)))
        .collect();

    let certified = CertifiedValidatorSetTransition::new(transition, votes, &current).unwrap();
    assert_eq!(certified.next_validator_set().version(), 5);
    assert_eq!(certified.next_validator_set().len(), 5);
}

#[test]
fn joining_validator_cannot_contribute_a_vote_to_its_admission_transition() {
    let current = current_set();
    let next = ValidatorSet::new(5, (1..=5).map(credential)).unwrap();
    let transition = ValidatorSetTransition::new(
        CURRENT_PROTOCOL_VERSION,
        &current,
        &registry(&current),
        next,
        vec![admission(5)],
        Vec::new(),
    )
    .unwrap();
    let statement = transition.finality_statement();

    let votes = vec![
        signed_vote(&statement, ValidatorId::new(1), &key(4)),
        signed_vote(&statement, ValidatorId::new(2), &key(7)),
        signed_vote(&statement, ValidatorId::new(5), &key(16)),
    ];

    assert_eq!(
        CertifiedValidatorSetTransition::new(transition, votes, &current),
        Err(ValidatorTransitionError::Finality(
            second::FinalityError::UnknownValidator(ValidatorId::new(5))
        ))
    );
}

#[test]
fn retained_validator_identity_key_cannot_be_rewritten() {
    let current = current_set();

    let mut next_credentials = (1..=4).map(credential).collect::<Vec<_>>();
    next_credentials[0] = ValidatorCredential::new(
        ValidatorId::new(1),
        key(90).verifying_key().to_bytes(),
        key(4).verifying_key().to_bytes(),
        key(5).verifying_key().to_bytes(),
    )
    .unwrap();

    let next = ValidatorSet::new(5, next_credentials).unwrap();

    assert_eq!(
        ValidatorSetTransition::new(
            CURRENT_PROTOCOL_VERSION,
            &current,
            &registry(&current),
            next,
            Vec::new(),
            Vec::new(),
        ),
        Err(ValidatorTransitionError::IdentityKeyChanged(
            ValidatorId::new(1)
        ))
    );
}

#[test]
fn retained_validator_recovery_key_cannot_be_rewritten() {
    let current = current_set();

    let mut next_credentials = (1..=4).map(credential).collect::<Vec<_>>();
    next_credentials[0] = ValidatorCredential::new(
        ValidatorId::new(1),
        key(3).verifying_key().to_bytes(),
        key(4).verifying_key().to_bytes(),
        key(90).verifying_key().to_bytes(),
    )
    .unwrap();
    let next = ValidatorSet::new(5, next_credentials).unwrap();

    assert_eq!(
        ValidatorSetTransition::new(
            CURRENT_PROTOCOL_VERSION,
            &current,
            &registry(&current),
            next,
            Vec::new(),
            Vec::new(),
        ),
        Err(ValidatorTransitionError::RecoveryKeyChanged(
            ValidatorId::new(1)
        ))
    );
}

#[test]
fn retained_validator_consensus_key_requires_rotation_request() {
    let current = current_set();

    let mut next_credentials = (1..=4).map(credential).collect::<Vec<_>>();
    next_credentials[0] = ValidatorCredential::new(
        ValidatorId::new(1),
        key(3).verifying_key().to_bytes(),
        key(90).verifying_key().to_bytes(),
        key(5).verifying_key().to_bytes(),
    )
    .unwrap();
    let next = ValidatorSet::new(5, next_credentials).unwrap();

    assert_eq!(
        ValidatorSetTransition::new(
            CURRENT_PROTOCOL_VERSION,
            &current,
            &registry(&current),
            next,
            Vec::new(),
            Vec::new(),
        ),
        Err(ValidatorTransitionError::MissingConsensusKeyRotation(
            ValidatorId::new(1)
        ))
    );
}

#[test]
fn identity_authorized_consensus_key_rotation_is_accepted() {
    let current = current_set();

    let mut next_credentials = (1..=4).map(credential).collect::<Vec<_>>();
    next_credentials[0] = ValidatorCredential::new(
        ValidatorId::new(1),
        key(3).verifying_key().to_bytes(),
        key(90).verifying_key().to_bytes(),
        key(5).verifying_key().to_bytes(),
    )
    .unwrap();
    let next = ValidatorSet::new(5, next_credentials).unwrap();

    let rotation = ValidatorConsensusKeyRotationRequest::sign(
        CURRENT_PROTOCOL_VERSION,
        ValidatorRotationAuthority::Identity,
        ValidatorId::new(1),
        4,
        key(90).verifying_key().to_bytes(),
        &key(3),
    )
    .unwrap();

    let transition = ValidatorSetTransition::new(
        CURRENT_PROTOCOL_VERSION,
        &current,
        &registry(&current),
        next,
        Vec::new(),
        vec![rotation],
    )
    .unwrap();

    assert_eq!(
        transition
            .next_validator_set()
            .validator(ValidatorId::new(1))
            .unwrap()
            .consensus_public_key(),
        key(90).verifying_key().to_bytes()
    );
}

#[test]
fn consensus_key_rotation_is_bound_to_current_validator_set_version() {
    let current = current_set();

    let mut next_credentials = (1..=4).map(credential).collect::<Vec<_>>();
    next_credentials[0] = ValidatorCredential::new(
        ValidatorId::new(1),
        key(3).verifying_key().to_bytes(),
        key(90).verifying_key().to_bytes(),
        key(5).verifying_key().to_bytes(),
    )
    .unwrap();
    let next = ValidatorSet::new(5, next_credentials).unwrap();

    let rotation = ValidatorConsensusKeyRotationRequest::sign(
        CURRENT_PROTOCOL_VERSION,
        ValidatorRotationAuthority::Recovery,
        ValidatorId::new(1),
        6,
        key(90).verifying_key().to_bytes(),
        &key(5),
    )
    .unwrap();

    assert_eq!(
        ValidatorSetTransition::new(
            CURRENT_PROTOCOL_VERSION,
            &current,
            &registry(&current),
            next,
            Vec::new(),
            vec![rotation],
        ),
        Err(
            ValidatorTransitionError::ConsensusKeyRotationValidatorSetMismatch {
                validator_id: ValidatorId::new(1),
                expected: 4,
                actual: 6,
            }
        )
    );
}

#[test]
fn newly_added_validator_without_admission_proof_is_rejected() {
    let current = current_set();
    let next = ValidatorSet::new(5, (1..=5).map(credential)).unwrap();

    assert_eq!(
        ValidatorSetTransition::new(
            CURRENT_PROTOCOL_VERSION,
            &current,
            &registry(&current),
            next,
            Vec::new(),
            Vec::new(),
        ),
        Err(ValidatorTransitionError::MissingAdmission(
            ValidatorId::new(5)
        ))
    );
}

#[test]
fn admission_for_validator_not_added_to_next_set_is_rejected() {
    let current = current_set();
    let next = ValidatorSet::new(5, (1..=4).map(credential)).unwrap();

    assert_eq!(
        ValidatorSetTransition::new(
            CURRENT_PROTOCOL_VERSION,
            &current,
            &registry(&current),
            next,
            vec![admission(6)],
            Vec::new(),
        ),
        Err(ValidatorTransitionError::UnexpectedAdmission(
            ValidatorId::new(6)
        ))
    );
}

#[test]
fn duplicate_admission_proof_is_rejected() {
    let current = current_set();
    let next = ValidatorSet::new(5, (1..=5).map(credential)).unwrap();
    let proof = admission(5);

    assert_eq!(
        ValidatorSetTransition::new(
            CURRENT_PROTOCOL_VERSION,
            &current,
            &registry(&current),
            next,
            vec![proof.clone(), proof],
            Vec::new(),
        ),
        Err(ValidatorTransitionError::DuplicateAdmission(
            ValidatorId::new(5)
        ))
    );
}

#[test]
fn certified_transition_activates_immediate_next_validator_set() {
    let current = current_set();
    let next = ValidatorSet::new(5, (1..=5).map(credential)).unwrap();
    let transition = ValidatorSetTransition::new(
        CURRENT_PROTOCOL_VERSION,
        &current,
        &registry(&current),
        next,
        vec![admission(5)],
        Vec::new(),
    )
    .unwrap();
    let statement = transition.finality_statement();
    let votes = [1_u64, 2, 3]
        .into_iter()
        .map(|id| signed_vote(&statement, ValidatorId::new(id), &key((id * 3 + 1) as u8)))
        .collect();
    let certified = CertifiedValidatorSetTransition::new(transition, votes, &current).unwrap();
    let mut registry = registry(&current);

    let activated = certified.activate(&mut registry).unwrap();
    assert_eq!(activated.version(), 5);
    assert_eq!(activated.len(), 5);
}

#[test]
fn prepared_task_can_finish_with_retained_validator_set_after_durable_activation() {
    let current = current_set();
    let next = ValidatorSet::new(5, (1..=5).map(credential)).unwrap();
    let transition = ValidatorSetTransition::new(
        CURRENT_PROTOCOL_VERSION,
        &current,
        &registry(&current),
        next,
        vec![admission(5)],
        Vec::new(),
    )
    .unwrap();
    let transition_statement = transition.finality_statement();
    let transition_votes = [1_u64, 2, 3]
        .into_iter()
        .map(|id| {
            signed_vote(
                &transition_statement,
                ValidatorId::new(id),
                &key((id * 3 + 1) as u8),
            )
        })
        .collect();
    let certified =
        CertifiedValidatorSetTransition::new(transition, transition_votes, &current).unwrap();

    let alice = support::account(1);
    let store = StateStore::new(temp_base("prepared-cross-validator-set"));
    let mut state = SecondState::genesis([alice], 1);
    let task = verified_task(
        900,
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );
    {
        let mut book = PreparedTaskBook::new(store.clone()).unwrap();
        book.prepare(&mut state, &task, 1, &current).unwrap();
    }

    store.activate_validator_set_transition(&certified).unwrap();

    let activated_v5 = store.load().unwrap().unwrap();
    assert_eq!(activated_v5.validator_set.version(), 5);
    assert_eq!(activated_v5.retained_validator_sets.get(&4), Some(&current));

    let current_v5 = activated_v5.validator_set.clone();
    let next_v6 = ValidatorSet::new(6, (1..=5).map(credential)).unwrap();
    let transition_v6 = ValidatorSetTransition::new(
        CURRENT_PROTOCOL_VERSION,
        &current_v5,
        &activated_v5.validator_registry,
        next_v6,
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    let statement_v6 = transition_v6.finality_statement();
    let votes_v6 = [1_u64, 2, 3, 4]
        .into_iter()
        .map(|id| {
            signed_vote(
                &statement_v6,
                ValidatorId::new(id),
                &key((id * 3 + 1) as u8),
            )
        })
        .collect();
    let certified_v6 =
        CertifiedValidatorSetTransition::new(transition_v6, votes_v6, &current_v5).unwrap();
    store
        .activate_validator_set_transition(&certified_v6)
        .unwrap();

    let activated = store.load().unwrap().unwrap();
    assert_eq!(activated.validator_set.version(), 6);
    assert_eq!(activated.retained_validator_sets.get(&4), Some(&current));
    assert!(!activated.retained_validator_sets.contains_key(&5));

    let stale_checkpoint = PublicCurrencyCheckpoint::new(
        CURRENT_PROTOCOL_VERSION,
        12,
        activated.state.public_currency_summary(),
    );
    assert_eq!(
        ValidatorSigner::new(ValidatorId::new(1), key(4), store.clone())
            .sign_public_checkpoint(&stale_checkpoint, &current),
        Err(ValidatorSigningError::Persistence(
            PersistenceError::ValidatorRegistryMismatch
        ))
    );

    let mut state = activated.state;
    let mut recovered = PreparedTaskBook::new(store.clone()).unwrap();
    let statement = recovered
        .prepared_finality_statement(task.task_id())
        .unwrap();
    assert_eq!(statement.validator_set_version(), 4);

    recovered
        .sign_prepared_vote(task.task_id(), ValidatorId::new(1), &key(4))
        .unwrap();

    let certificate = certificate_from_keys(
        statement,
        &current,
        [1_u64, 2, 3]
            .into_iter()
            .map(|id| (ValidatorId::new(id), key((id * 3 + 1) as u8))),
    );
    assert_eq!(
        recovered
            .commit(&mut state, task.task_id(), &certificate)
            .unwrap(),
        ExecutionOutcome::Succeeded
    );

    let committed = store.load().unwrap().unwrap();
    assert_eq!(committed.validator_set.version(), 6);
    assert!(committed.retained_validator_sets.is_empty());
    assert_eq!(committed.state.current_supply(), 1);

    store.remove_files().unwrap();
}
