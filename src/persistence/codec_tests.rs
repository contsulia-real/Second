use std::collections::BTreeMap;

use super::snapshot_validation::{
    validate_prepared_plans_against_state, validate_prepared_snapshot_links,
    validate_vote_lock_registry,
};
use crate::payment::{EstablishedTransfer, PaymentExecution};
use crate::prepared_plan::{PreparedOperation, PreparedTask, PreparedTaskPhase};
use crate::state::TaskBinding;
use crate::validator_signer::FinalityScope;
use crate::{
    AccountAddress, CurrencyAddress, OperationClaimId, PaymentAddress, PersistenceError,
    PreparationError, SecondState, TaskId, ValidatorCredential, ValidatorId, ValidatorRegistry,
    ValidatorSet,
};

#[test]
fn prepared_snapshot_links_reject_tampered_identity_or_transfer_state() {
    let task_id = TaskId::parse("prepared-task").unwrap();
    let request_digest = [7; 32];
    let source = PaymentAddress::from_bytes([1; 32]);
    let destination = PaymentAddress::from_bytes([2; 32]);
    let source_account = AccountAddress::from_bytes([3; 32]);
    let destination_account = AccountAddress::from_bytes([4; 32]);
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
            request_digest,
            succeeded: false,
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
            request_digest: [8; 32],
            succeeded: false,
        },
    )]);
    assert_eq!(
        validate_prepared_snapshot_links(
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
    let request_digest = [7; 32];
    let account = AccountAddress::from_bytes([3; 32]);
    let bindings = BTreeMap::from([(
        task_id.clone(),
        TaskBinding {
            request_digest,
            succeeded: false,
        },
    )]);
    let mut prepared_task = PreparedTask::new(
        task_id.clone(),
        request_digest,
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
            FinalityScope::PreparedTask(task_id.clone()),
        ),
        plan_digest,
    )]);
    assert_eq!(
        validate_prepared_snapshot_links(
            &bindings,
            &BTreeMap::new(),
            &executions,
            &prepared,
            &valid_lock,
        ),
        Ok(())
    );

    let wrong_lock = BTreeMap::from([(
        (ValidatorId::new(1), FinalityScope::PreparedTask(task_id)),
        [9; 32],
    )]);
    assert_eq!(
        validate_prepared_snapshot_links(
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
            FinalityScope::PreparedTask(task_id.clone()),
        ),
        [7; 32],
    )]);
    let prepared = BTreeMap::new();
    let executions = BTreeMap::new();

    assert_eq!(
        validate_prepared_snapshot_links(
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
            request_digest: [8; 32],
            succeeded: false,
        },
    )]);
    assert_eq!(
        validate_prepared_snapshot_links(
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
            request_digest: [8; 32],
            succeeded: true,
        },
    )]);
    assert_eq!(
        validate_prepared_snapshot_links(
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
    let source_account = AccountAddress::from_bytes([3; 32]);
    let destination_account = AccountAddress::from_bytes([4; 32]);
    let prepared = PreparedTask::new(
        task_id,
        [7; 32],
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
    let account = AccountAddress::from_bytes([3; 32]);
    let state = SecondState::genesis([account], 10);

    let valid = BTreeMap::from([(
        TaskId::parse("valid-preallocation").unwrap(),
        PreparedTask::new(
            TaskId::parse("valid-preallocation").unwrap(),
            [1; 32],
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
                first_id,
                [3; 32],
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
                second_id,
                [4; 32],
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
            FinalityScope::PublicCheckpoint {
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
            FinalityScope::PublicCheckpoint {
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
