use std::collections::BTreeMap;

use super::snapshot_validation::{
    validate_active_prepared_vote_lock_membership, validate_prepared_plans_against_state,
    validate_prepared_snapshot_links, validate_vote_lock_registry,
};
use crate::ConsensusScope;
use crate::payment::{EstablishedTransfer, PaymentExecution};
use crate::prepared_plan::{PreparedOperation, PreparedTask, PreparedTaskPhase};
use crate::state::TaskBinding;
use crate::{
    CURRENT_PROTOCOL_VERSION, CurrencyAddress, LegalTask, LegalTaskPayload, Operation,
    OperationClaimId, PaymentAddress, PersistenceError, PreparationError, SecondState, TaskId,
    ValidatorCredential, ValidatorId, ValidatorRegistry, ValidatorSet,
};

fn prepared_source(task_id: &TaskId) -> (LegalTask, [u8; 32]) {
    let payload = LegalTaskPayload::new(
        task_id.clone(),
        CURRENT_PROTOCOL_VERSION,
        None,
        vec![Operation::Issue {
            account: crate::test_helpers::account(31),
            count: 1,
        }],
    );
    let task =
        crate::test_helpers::sign(payload, &ed25519_dalek::SigningKey::from_bytes(&[32; 32]))
            .expect("test LegalTask must encode");
    let digest = task.request_digest().expect("test LegalTask must encode");
    (task, digest)
}

#[test]
fn prepared_snapshot_links_reject_tampered_identity_or_transfer_state() {
    let task_id = TaskId::parse("prepared-task").unwrap();
    let (source_task, request_digest) = prepared_source(&task_id);
    let source = PaymentAddress::from_bytes([1; 32]);
    let destination = PaymentAddress::from_bytes([2; 32]);
    let source_account = crate::test_helpers::account(3);
    let destination_account = crate::test_helpers::account(4);
    let transfer = EstablishedTransfer {
        source,
        destination,
        source_account,
        destination_account,
        amount: 1,
    };
    let claim_id = OperationClaimId::new(task_id.clone(), 0);

    let bindings = BTreeMap::from([(
        task_id.clone(),
        TaskBinding {
            allocation: None,
            allocation_task: None,
            allocation_certificate: None,
            request_digest,
            outcome: crate::state::TaskOutcome::Pending,
        },
    )]);
    let payment_addresses = BTreeMap::from([
        (
            source,
            crate::payment::PaymentAddressRecord {
                account: source_account,
                status: crate::PaymentAddressStatus::Active,
            },
        ),
        (
            destination,
            crate::payment::PaymentAddressRecord {
                account: destination_account,
                status: crate::PaymentAddressStatus::Active,
            },
        ),
    ]);
    let executions = BTreeMap::from([(
        claim_id.clone(),
        PaymentExecution {
            source,
            destination,
            amount: 1,
        },
    )]);
    let prepared = BTreeMap::from([(
        task_id.clone(),
        PreparedTask::new(
            task_id.clone(),
            request_digest,
            source_task,
            1,
            vec![PreparedOperation::Transfer {
                transfer,
                currencies: vec![CurrencyAddress::new(1)],
            }],
        ),
    )]);

    let vote_locks = BTreeMap::new();

    assert_eq!(
        validate_prepared_snapshot_links(
            &crate::prepared::tests::validator_set(),
            &BTreeMap::new(),
            &bindings,
            &payment_addresses,
            &executions,
            &prepared,
            &vote_locks,
        ),
        Ok(())
    );

    let wrong_bindings = BTreeMap::from([(
        task_id.clone(),
        TaskBinding {
            allocation: None,
            allocation_task: None,
            allocation_certificate: None,
            request_digest: [8; 32],
            outcome: crate::state::TaskOutcome::Pending,
        },
    )]);
    assert_eq!(
        validate_prepared_snapshot_links(
            &crate::prepared::tests::validator_set(),
            &BTreeMap::new(),
            &wrong_bindings,
            &payment_addresses,
            &executions,
            &prepared,
            &vote_locks,
        ),
        Err(PersistenceError::InvalidSnapshot)
    );

    let mut wrong_execution = executions.clone();
    wrong_execution.get_mut(&claim_id).unwrap().amount = 2;
    assert_eq!(
        validate_prepared_snapshot_links(
            &crate::prepared::tests::validator_set(),
            &BTreeMap::new(),
            &bindings,
            &payment_addresses,
            &wrong_execution,
            &prepared,
            &vote_locks
        ),
        Err(PersistenceError::InvalidSnapshot)
    );
}

#[test]
fn prepared_snapshot_links_reject_vote_lock_for_a_different_plan_digest() {
    let task_id = TaskId::parse("vote-lock-plan").unwrap();
    let (source_task, request_digest) = prepared_source(&task_id);
    let account = crate::test_helpers::account(3);
    let bindings = BTreeMap::from([(
        task_id.clone(),
        TaskBinding {
            allocation: None,
            allocation_task: None,
            allocation_certificate: None,
            request_digest,
            outcome: crate::state::TaskOutcome::Pending,
        },
    )]);
    let mut prepared_task = PreparedTask::new(
        task_id.clone(),
        request_digest,
        source_task,
        1,
        vec![PreparedOperation::Issue {
            account,
            addresses: vec![CurrencyAddress::new(1)],
        }],
    );
    prepared_task.advance_phase(PreparedTaskPhase::Voting);
    let plan_digest = prepared_task.plan_digest().unwrap();
    let prepared = BTreeMap::from([(task_id.clone(), prepared_task)]);
    let executions = BTreeMap::new();

    let valid_lock = BTreeMap::from([(
        (
            ValidatorId::new(1),
            ConsensusScope::PreparedTask(task_id.clone()),
        ),
        plan_digest,
    )]);
    assert_eq!(
        validate_prepared_snapshot_links(
            &crate::prepared::tests::validator_set(),
            &BTreeMap::new(),
            &bindings,
            &BTreeMap::new(),
            &executions,
            &prepared,
            &valid_lock,
        ),
        Ok(())
    );

    let wrong_lock = BTreeMap::from([(
        (ValidatorId::new(1), ConsensusScope::PreparedTask(task_id)),
        [9; 32],
    )]);
    assert_eq!(
        validate_prepared_snapshot_links(
            &crate::prepared::tests::validator_set(),
            &BTreeMap::new(),
            &bindings,
            &BTreeMap::new(),
            &executions,
            &prepared,
            &wrong_lock,
        ),
        Err(PersistenceError::InvalidSnapshot)
    );
}

#[test]
fn prepared_vote_lock_without_active_plan_requires_succeeded_binding() {
    let task_id = TaskId::parse("completed-vote-lock").unwrap();
    let vote_locks = BTreeMap::from([(
        (
            ValidatorId::new(1),
            ConsensusScope::PreparedTask(task_id.clone()),
        ),
        [7; 32],
    )]);
    let prepared = BTreeMap::new();
    let executions = BTreeMap::new();

    assert_eq!(
        validate_prepared_snapshot_links(
            &crate::prepared::tests::validator_set(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &executions,
            &prepared,
            &vote_locks,
        ),
        Err(PersistenceError::InvalidSnapshot)
    );

    let unfinished = BTreeMap::from([(
        task_id.clone(),
        TaskBinding {
            allocation: None,
            allocation_task: None,
            allocation_certificate: None,
            request_digest: [8; 32],
            outcome: crate::state::TaskOutcome::Pending,
        },
    )]);
    assert_eq!(
        validate_prepared_snapshot_links(
            &crate::prepared::tests::validator_set(),
            &BTreeMap::new(),
            &unfinished,
            &BTreeMap::new(),
            &executions,
            &prepared,
            &vote_locks,
        ),
        Err(PersistenceError::InvalidSnapshot)
    );

    let succeeded = BTreeMap::from([(
        task_id,
        TaskBinding {
            allocation: None,
            allocation_task: None,
            allocation_certificate: None,
            request_digest: [8; 32],
            outcome: crate::state::TaskOutcome::Succeeded,
        },
    )]);
    assert_eq!(
        validate_prepared_snapshot_links(
            &crate::prepared::tests::validator_set(),
            &BTreeMap::new(),
            &succeeded,
            &BTreeMap::new(),
            &executions,
            &prepared,
            &vote_locks,
        ),
        Ok(())
    );
}

#[test]
fn prepared_transfer_rejects_currency_count_different_from_frozen_amount() {
    let task_id = TaskId::parse("transfer-count-mismatch").unwrap();
    let source = PaymentAddress::from_bytes([1; 32]);
    let destination = PaymentAddress::from_bytes([2; 32]);
    let source_account = crate::test_helpers::account(3);
    let destination_account = crate::test_helpers::account(4);
    let (source_task, _) = prepared_source(&task_id);
    let prepared = PreparedTask::new(
        task_id,
        [7; 32],
        source_task,
        1,
        vec![PreparedOperation::Transfer {
            transfer: EstablishedTransfer {
                source,
                destination,
                source_account,
                destination_account,
                amount: 2,
            },
            currencies: vec![CurrencyAddress::new(1)],
        }],
    );
    let mut state = SecondState::genesis([source_account, destination_account], 10);
    let mut working = state.business.clone();

    assert_eq!(
        prepared.apply(&mut state, &mut working),
        Err(PreparationError::InvalidPreparedPlan)
    );
}

#[test]
fn restored_preallocated_currency_plans_must_fit_frontier_and_be_globally_unique() {
    let account = crate::test_helpers::account(3);
    let state = SecondState::genesis([account], 10);

    let valid = BTreeMap::from([(
        TaskId::parse("valid-preallocation").unwrap(),
        PreparedTask::new(
            TaskId::parse("valid-preallocation").unwrap(),
            [1; 32],
            prepared_source(&TaskId::parse("valid-preallocation").unwrap()).0,
            1,
            vec![PreparedOperation::Issue {
                account,
                addresses: vec![CurrencyAddress::new(9)],
            }],
        ),
    )]);
    assert_eq!(
        validate_prepared_plans_against_state(&state, &valid),
        Ok(())
    );

    let beyond_frontier = BTreeMap::from([(
        TaskId::parse("bad-frontier").unwrap(),
        PreparedTask::new(
            TaskId::parse("bad-frontier").unwrap(),
            [2; 32],
            prepared_source(&TaskId::parse("bad-frontier").unwrap()).0,
            1,
            vec![PreparedOperation::Issue {
                account,
                addresses: vec![CurrencyAddress::new(10)],
            }],
        ),
    )]);
    assert_eq!(
        validate_prepared_plans_against_state(&state, &beyond_frontier),
        Err(PersistenceError::InvalidSnapshot)
    );

    let first_id = TaskId::parse("duplicate-preallocation-a").unwrap();
    let second_id = TaskId::parse("duplicate-preallocation-b").unwrap();
    let duplicate = BTreeMap::from([
        (
            first_id.clone(),
            PreparedTask::new(
                first_id.clone(),
                [3; 32],
                prepared_source(&first_id).0,
                1,
                vec![PreparedOperation::Issue {
                    account,
                    addresses: vec![CurrencyAddress::new(8)],
                }],
            ),
        ),
        (
            second_id.clone(),
            PreparedTask::new(
                second_id.clone(),
                [4; 32],
                prepared_source(&second_id).0,
                1,
                vec![PreparedOperation::Issue {
                    account,
                    addresses: vec![CurrencyAddress::new(8)],
                }],
            ),
        ),
    ]);
    assert_eq!(
        validate_prepared_plans_against_state(&state, &duplicate),
        Err(PersistenceError::InvalidSnapshot)
    );
}

#[test]
fn vote_locks_require_a_validator_from_registry_history() {
    let identity = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
    let consensus = ed25519_dalek::SigningKey::from_bytes(&[2; 32]);
    let recovery = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
    let credential = ValidatorCredential::new(
        ValidatorId::new(1),
        identity.verifying_key().to_bytes(),
        consensus.verifying_key().to_bytes(),
        recovery.verifying_key().to_bytes(),
    )
    .unwrap();
    let set = ValidatorSet::new(1, [credential]).unwrap();
    let registry = ValidatorRegistry::from_validator_set(&set).unwrap();

    let known = BTreeMap::from([(
        (
            ValidatorId::new(1),
            ConsensusScope::PublicCheckpoint {
                validator_set_version: 1,
                epoch: 7,
            },
        ),
        [4; 32],
    )]);
    assert_eq!(validate_vote_lock_registry(&registry, &known), Ok(()));

    let unknown = BTreeMap::from([(
        (
            ValidatorId::new(2),
            ConsensusScope::PublicCheckpoint {
                validator_set_version: 1,
                epoch: 7,
            },
        ),
        [4; 32],
    )]);
    assert_eq!(
        validate_vote_lock_registry(&registry, &unknown),
        Err(PersistenceError::InvalidSnapshot)
    );
}

#[test]
fn active_prepared_vote_lock_requires_member_of_bound_validator_set() {
    let identity = ed25519_dalek::SigningKey::from_bytes(&[11; 32]);
    let consensus = ed25519_dalek::SigningKey::from_bytes(&[12; 32]);
    let recovery = ed25519_dalek::SigningKey::from_bytes(&[13; 32]);
    let credential = ValidatorCredential::new(
        ValidatorId::new(1),
        identity.verifying_key().to_bytes(),
        consensus.verifying_key().to_bytes(),
        recovery.verifying_key().to_bytes(),
    )
    .unwrap();
    let set = ValidatorSet::new(1, [credential]).unwrap();
    let task_id = TaskId::parse("vote-lock-membership").unwrap();
    let mut prepared_task = PreparedTask::new(
        task_id.clone(),
        [7; 32],
        prepared_source(&task_id).0,
        1,
        vec![PreparedOperation::Issue {
            account: crate::test_helpers::account(3),
            addresses: vec![CurrencyAddress::new(1)],
        }],
    );
    prepared_task.advance_phase(PreparedTaskPhase::Voting);
    let digest = prepared_task.plan_digest().unwrap();
    let prepared = BTreeMap::from([(task_id.clone(), prepared_task)]);

    let valid = BTreeMap::from([(
        (
            ValidatorId::new(1),
            ConsensusScope::PreparedTask(task_id.clone()),
        ),
        digest,
    )]);
    assert_eq!(
        validate_active_prepared_vote_lock_membership(&set, &BTreeMap::new(), &prepared, &valid,),
        Ok(())
    );

    let invalid = BTreeMap::from([(
        (ValidatorId::new(2), ConsensusScope::PreparedTask(task_id)),
        digest,
    )]);
    assert_eq!(
        validate_active_prepared_vote_lock_membership(&set, &BTreeMap::new(), &prepared, &invalid,),
        Err(PersistenceError::InvalidSnapshot)
    );
}
