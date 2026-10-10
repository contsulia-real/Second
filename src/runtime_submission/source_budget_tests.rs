use crate::legal_task_codec::{MAX_ENCODED_LEGAL_TASK_SIZE, encode_legal_task};
use crate::prepared::source::{
    MAX_PREPARED_SOURCE_SIZE, PreparedTaskSource, encoded_source_length,
};
use crate::prepared::tests::{key, temp_store, validator_set};
use crate::prepared_plan::{PreparedOperation, PreparedTask};
use crate::runtime_bft::ValidatorBftRuntime;
use crate::runtime_submission::LegalTaskSubmissionContext;
use crate::*;
use std::time::Duration;

fn signed(name: &str, operations: Vec<Operation>) -> LegalTask {
    let mut task = LegalTask::sign(
        LegalTaskPayload::new(TaskId::parse(name).unwrap(), 1, None, operations),
        &key(9),
    )
    .unwrap();
    task.add_account_signature(&key(61)).unwrap();
    task
}

fn repair(count: usize) -> Operation {
    Operation::LeakRepair {
        leaked: (0..count as u64).map(CurrencyAddress::new).collect(),
    }
}

#[test]
fn leak_repair_admission_limit_counts_addresses_across_operations() {
    let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
    let maximum = MAX_LEAK_REPAIR_ADDRESSES_PER_TASK;
    for counts in [vec![maximum], vec![maximum / 2, maximum / 2]] {
        signed("repair-limit", counts.into_iter().map(repair).collect())
            .verify(&authorizers)
            .unwrap();
    }
    for counts in [vec![maximum + 1], vec![maximum / 2, maximum / 2 + 1]] {
        assert_eq!(
            signed("repair-limit", counts.into_iter().map(repair).collect()).verify(&authorizers),
            Err(AuthorizationError::InvalidPayload(
                TaskValidationError::TooManyLeakRepairAddresses {
                    maximum,
                    actual: maximum + 1,
                }
            ))
        );
    }
}

#[tokio::test]
async fn allocation_source_rejection_precedes_all_durable_admission() {
    let validators = validator_set();
    let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
    let (store, base) = temp_store();
    store
        .initialize(
            &SecondState::genesis([crate::test_helpers::account(61)], 1),
            &validators,
        )
        .unwrap();
    let runtime = ValidatorBftRuntime::new(
        ValidatorRuntimeKeys::new(ValidatorId::new(1), key(3), key(4)),
        ValidatorRuntimeConfig::new(
            authorizers.clone(),
            BftTimeoutConfig::new(
                Duration::from_secs(1),
                Duration::from_secs(1),
                Duration::from_secs(1),
            ),
            || 1,
        ),
        store.clone(),
        validators,
        std::iter::empty(),
    )
    .unwrap();
    let context = LegalTaskSubmissionContext::new(store.clone(), runtime);
    let before = store.load().unwrap().unwrap();
    for mut operations in [
        vec![repair(1)],
        vec![
            Operation::Issue {
                account: crate::test_helpers::account(61),
                count: 1,
            },
            Operation::Transfer {
                source: PaymentAddress::from_bytes([61; 32]),
                destination: PaymentAddress::from_bytes([62; 32]),
                amount: 1,
            },
        ],
    ] {
        operations.push(Operation::Destroy {
            currencies: vec![CurrencyAddress::new(3)],
        });
        let overhead = encoded_source_length(0, [1]).unwrap();
        let target_request_length = MAX_PREPARED_SOURCE_SIZE + 1 - overhead;
        let initial_length = encode_legal_task(&signed("a", operations.clone()))
            .unwrap()
            .len();
        // Align the request to the eight-byte Destroy addresses without changing limits.
        let name = "a".repeat(1 + (target_request_length - initial_length) % size_of::<u64>());
        let initial_length = encode_legal_task(&signed(&name, operations.clone()))
            .unwrap()
            .len();
        let count = 1 + (target_request_length - initial_length) / size_of::<u64>();
        *operations.last_mut().unwrap() = Operation::Destroy {
            currencies: (3..3 + count as u64).map(CurrencyAddress::new).collect(),
        };
        let task = signed(&name, operations.clone());
        let request_length = encode_legal_task(&task).unwrap().len();
        assert_eq!(request_length, target_request_length);
        assert!(request_length <= MAX_ENCODED_LEGAL_TASK_SIZE);
        task.payload().validate().unwrap();
        assert!(name.len() > 1);
        // One byte less is accepted, including the existing Issue + Transfer combination.
        signed(&name[..name.len() - 1], operations)
            .verify(&authorizers)
            .unwrap();
        let task_id = task.payload().task_id();
        assert!(matches!(
            context.submit(task),
            Err(NodeRuntimeError::Authorization(AuthorizationError::AllocationSourceTooLarge {
                maximum: MAX_PREPARED_SOURCE_SIZE,
                required,
            })) if required == MAX_PREPARED_SOURCE_SIZE + 1
        ));
        let cold = StateStore::new(&base).load().unwrap().unwrap();
        assert_eq!(cold.generation, before.generation);
        assert!(cold.state.same_persisted_state(&before.state));
        assert_eq!(cold.state.bound_request_digest(task_id), None);
        assert!(cold.state.protocol.task_bindings.is_empty());
        assert!(cold.prepared_tasks.is_empty());
        assert!(cold.validator_vote_locks.is_empty());
        assert!(cold.bft_local_states.is_empty());
    }
    drop(context);
    store.remove_files().unwrap();
    let _ = std::fs::remove_file(base.with_extension("lock"));
}

#[test]
fn maximum_pure_repair_source_fits_with_all_selections_fragmented() {
    let maximum = MAX_LEAK_REPAIR_ADDRESSES_PER_TASK;
    let owner = crate::test_helpers::account_u16(0);
    let mut operations = Vec::with_capacity(maximum);
    let mut prepared = Vec::with_capacity(maximum);
    for index in 0..maximum as u64 {
        let leaked = vec![CurrencyAddress::new(1_000_000 + index)];
        operations.push(Operation::LeakRepair {
            leaked: leaked.clone(),
        });
        prepared.push(PreparedOperation::LeakRepair {
            leaked,
            leaked_owners: vec![owner],
            reserve: [CurrencyAddress::new(2 * index + 1)].into_iter().collect(),
            replacement_reserve: [CurrencyAddress::new(2_000_000 + index)]
                .into_iter()
                .collect(),
        });
    }
    // Nonempty LeakRepair operations allow at most `maximum` operations here.
    let payload = LegalTaskPayload::new(
        TaskId::parse(&"a".repeat(128)).unwrap(),
        1,
        Some(u64::MAX),
        operations,
    );
    let mut task = LegalTask::sign(payload, &key(9)).unwrap();
    for index in 0..crate::authorization::MAX_ACCOUNT_SIGNATURES {
        task.add_account_signature(&crate::test_helpers::key_u16(index as u16))
            .unwrap();
    }
    let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
    let verified = task.verify(&authorizers).unwrap();
    let plan = PreparedTask::new(
        verified.task_id(),
        verified.request_digest(),
        task,
        1,
        prepared,
    );
    let bytes = plan.encode_source().unwrap();
    assert_eq!(bytes.len(), 639_258);
    assert_eq!(MAX_PREPARED_SOURCE_SIZE - bytes.len(), 1_457_898);
    let source = PreparedTaskSource::decode(&bytes).unwrap();
    assert_eq!(source.selections.len(), maximum);
    assert_eq!(
        source
            .selections
            .iter()
            .map(|selection| selection.ranges().len())
            .sum::<usize>(),
        maximum
    );
    println!(
        "pure LeakRepair worst source: {} bytes; headroom: {} bytes",
        bytes.len(),
        MAX_PREPARED_SOURCE_SIZE - bytes.len()
    );

    // Exercise actual preparation at the address limit with fragmented Reserve runs.
    let owner = crate::test_helpers::account(61);
    let mut state = SecondState::genesis([owner], 2_000_000);
    for index in 0..maximum as u64 {
        state.business.currencies.set_range(
            AddressRange::new(CurrencyAddress::new(2 * index + 1), 1).unwrap(),
            CurrencyRole::Reserve,
            None,
        );
    }
    state.business.currencies.set_range(
        AddressRange::new(CurrencyAddress::new(1_000_000), maximum as u64).unwrap(),
        CurrencyRole::Circulation,
        Some(owner),
    );
    let task = signed(
        "fragmented-repair-prepare",
        vec![Operation::LeakRepair {
            leaked: (1_000_000..1_000_000 + maximum as u64)
                .map(CurrencyAddress::new)
                .collect(),
        }],
    )
    .verify(&authorizers)
    .unwrap();
    let validators = validator_set();
    let (store, base) = temp_store();
    store.initialize(&state, &validators).unwrap();
    let allocation = CurrencyAllocation::new(&task, validators.version(), 2_000_000).unwrap();
    let statement = allocation.finality_statement();
    let votes = (1..=3)
        .map(|id| {
            ValidatorVote::sign_unchecked(
                &statement,
                ValidatorId::new(id),
                &key((id * 3 + 1) as u8),
            )
        })
        .collect();
    store
        .install_currency_allocation(
            &allocation,
            &FinalityCertificate::new(statement, votes, &validators).unwrap(),
        )
        .unwrap();
    state = store.load().unwrap().unwrap().state;
    let mut book = PreparedTaskBook::new(store.clone()).unwrap();
    assert_eq!(
        book.prepare(&mut state, &task, 1, &validators).unwrap(),
        PreparationOutcome::Prepared
    );
    let plan = store
        .load_prepared_tasks()
        .unwrap()
        .remove(&task.task_id())
        .unwrap();
    let bytes = plan.encode_source().unwrap();
    let decoded = PreparedTaskSource::decode(&bytes).unwrap();
    assert_eq!(decoded.selections[0].ranges().len(), maximum);
    println!(
        "actual fragmented Prepare source: {} bytes; headroom: {} bytes",
        bytes.len(),
        MAX_PREPARED_SOURCE_SIZE - bytes.len()
    );
    drop(book);
    store.remove_files().unwrap();
    let _ = std::fs::remove_file(base.with_extension("lock"));
}
