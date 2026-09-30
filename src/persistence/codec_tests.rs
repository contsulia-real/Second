use std::collections::BTreeMap;

use super::codec::validate_prepared_snapshot_links;
use crate::payment::{
    EstablishedTransfer, PaymentAddressRecord, PaymentAddressStatus, PaymentExecution,
};
use crate::prepared_plan::{PreparedOperation, PreparedTask};
use crate::state::TaskBinding;
use crate::{
    AccountAddress, CurrencyAddress, OperationClaimId, PaymentAddress, PersistenceError, TaskId,
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
            PaymentAddressRecord {
                account: source_account,
                status: PaymentAddressStatus::Active,
            },
        ),
        (
            destination,
            PaymentAddressRecord {
                account: destination_account,
                status: PaymentAddressStatus::Active,
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
            &vote_locks,
        ),
        Err(PersistenceError::InvalidSnapshot)
    );

    let mut wrong_addresses = payment_addresses;
    wrong_addresses.get_mut(&source).unwrap().account = AccountAddress::from_bytes([9; 32]);
    assert_eq!(
        validate_prepared_snapshot_links(
            &bindings,
            &wrong_addresses,
            &executions,
            &prepared,
            &vote_locks,
        ),
        Err(PersistenceError::InvalidSnapshot)
    );
}
