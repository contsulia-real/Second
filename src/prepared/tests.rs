use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use ed25519_dalek::SigningKey;

use super::*;
use crate::{
    AccountAddress, AuthorizerSet, FinalityCertificate, LegalTask, LegalTaskPayload, Operation,
    PreparationOutcome, TaskId, ValidatorCredential, ValidatorId, ValidatorSet, ValidatorVote,
};

pub(crate) fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

pub(crate) fn validator_set() -> ValidatorSet {
    ValidatorSet::new(
        1,
        (1..=4).map(|id| {
            ValidatorCredential::new(
                ValidatorId::new(id),
                key((id * 3) as u8).verifying_key().to_bytes(),
                key((id * 3 + 1) as u8).verifying_key().to_bytes(),
                key((id * 3 + 2) as u8).verifying_key().to_bytes(),
            )
            .unwrap()
        }),
    )
    .unwrap()
}

pub(crate) fn temp_store() -> (StateStore, std::path::PathBuf) {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let unique = format!(
        "second-prepared-finality-recovery-{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must be after unix epoch")
            .as_nanos(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed),
    );
    let base = std::env::temp_dir().join(unique);
    (StateStore::new(&base), base)
}

pub(crate) fn advance_nil_round(
    store: &StateStore,
    validators: &ValidatorSet,
    validator: ValidatorId,
    scope: crate::ConsensusScope,
    round: u64,
) {
    let statement = crate::BftStatement::new(
        validators.version(),
        scope,
        round,
        crate::BftPhase::Precommit,
        crate::BftValue::Nil,
    );
    let votes = validators
        .credentials()
        .take(validators.quorum_threshold())
        .map(|credential| {
            let id = credential.id();
            crate::BftVote::sign_unchecked(&statement, id, &key((id.value() * 3 + 1) as u8))
        })
        .collect();
    let certificate = crate::BftQuorumCertificate::new(statement, votes, validators).unwrap();
    store
        .accept_bft_nil_precommit_qc(validator, &certificate, validators)
        .unwrap();
}

#[test]
fn durable_finality_recovers_commit_after_crash_boundary() {
    let validators = validator_set();
    let account = AccountAddress::from_bytes([41; 32]);
    let (store, base) = temp_store();
    store
        .initialize(&SecondState::genesis([account], 1), &validators)
        .unwrap();

    let authorizer = key(9);
    let authorizers = AuthorizerSet::new(
        CURRENT_PROTOCOL_VERSION,
        [authorizer.verifying_key().to_bytes()],
    )
    .unwrap();
    let task_id = TaskId::parse("finality-recovery").unwrap();
    let signed = LegalTask::sign(
        LegalTaskPayload::new(
            task_id.clone(),
            CURRENT_PROTOCOL_VERSION,
            None,
            vec![Operation::Issue { account, count: 1 }],
        ),
        &authorizer,
    )
    .unwrap();
    let verified = signed.verify(&authorizers).unwrap();

    let allocation = crate::CurrencyAllocation::new(&verified, validators.version(), 1).unwrap();
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

    let persisted = store.load().unwrap().unwrap();
    let mut state = persisted.state;
    let mut book = PreparedTaskBook::new(store.clone()).unwrap();
    assert_eq!(
        book.prepare(&mut state, &verified, 1, &validators).unwrap(),
        PreparationOutcome::Prepared
    );
    let digest = book.prepared_plan_digest(task_id.clone()).unwrap();
    let statement = book.prepared_finality_statement(task_id.clone()).unwrap();
    let votes = (1..=3)
        .map(|id| {
            ValidatorVote::sign_unchecked(
                &statement,
                ValidatorId::new(id),
                &key((id * 3 + 1) as u8),
            )
        })
        .collect::<Vec<_>>();
    let certificate = FinalityCertificate::new(statement, votes, &validators).unwrap();

    store
        .finalize_prepared_task(&task_id, digest, &certificate)
        .unwrap();

    let finalized = store.load().unwrap().unwrap();
    assert_eq!(finalized.state.current_supply(), 0);
    assert!(finalized.prepared_tasks.contains_key(&task_id));
    assert!(!finalized.task_receipts.contains_key(&task_id));
    drop(book);

    PreparedTaskBook::recover_finalized_from_store(&store).unwrap();

    let recovered = store.load().unwrap().unwrap();
    assert_eq!(recovered.state.current_supply(), 1);
    assert!(!recovered.prepared_tasks.contains_key(&task_id));
    assert_eq!(
        recovered.task_receipts[&task_id]
            .certificate()
            .unwrap()
            .statement(),
        certificate.statement()
    );
    assert_eq!(
        PreparedTaskBook::new(store.clone())
            .unwrap()
            .claimed_currency_count(),
        0
    );

    store.remove_files().unwrap();
    let _ = std::fs::remove_file(base.with_extension("lock"));
}

#[test]
fn account_owner_signing_is_required_for_spending_even_with_a_trusted_authorizer() {
    use crate::currency::Currency;
    use crate::payment::PaymentAddressRecord;
    use crate::{
        CurrencyAddress, CurrencyRole, ExecutionError, PaymentAddress, PaymentAddressStatus,
    };

    let alice_key = key(51);
    let bob_key = key(52);
    let alice = AccountAddress::from_bytes(alice_key.verifying_key().to_bytes());
    let bob = AccountAddress::from_bytes(bob_key.verifying_key().to_bytes());
    let source = PaymentAddress::from_bytes([91; 32]);
    let destination = PaymentAddress::from_bytes([92; 32]);
    let mut state = SecondState::genesis([alice, bob], 2);
    for (address, account) in [(source, alice), (destination, bob)] {
        state.business.payment_addresses.insert(
            address,
            PaymentAddressRecord {
                account,
                status: PaymentAddressStatus::Active,
            },
        );
    }
    state.business.currencies.insert(
        CurrencyAddress::new(1),
        Currency {
            address: CurrencyAddress::new(1),
            role: CurrencyRole::Circulation,
            owner: Some(alice),
        },
    );
    let (store, base) = temp_store();
    let validators = validator_set();
    store.initialize(&state, &validators).unwrap();
    let authorizer = key(9);
    let authorizers = AuthorizerSet::new(1, [authorizer.verifying_key().to_bytes()]).unwrap();
    let payload = LegalTaskPayload::new(
        TaskId::parse("owner-auth-transfer").unwrap(),
        1,
        None,
        vec![Operation::Transfer {
            source,
            destination,
            amount: 1,
        }],
    );
    let mut book = PreparedTaskBook::new(store.clone()).unwrap();
    let admin_only = LegalTask::sign(payload.clone(), &authorizer)
        .unwrap()
        .verify(&authorizers)
        .unwrap();
    assert_eq!(
        book.prepare(&mut state, &admin_only, 1, &validators),
        Err(PreparationError::Execution(
            ExecutionError::MissingAccountSignature(alice)
        )),
    );
    assert_eq!(state.bound_request_digest(admin_only.task_id()), None);
    let wrong_owner = LegalTask::sign_account(payload.clone(), &bob_key)
        .unwrap()
        .verify(&authorizers)
        .unwrap();
    assert_eq!(
        book.prepare(&mut state, &wrong_owner, 1, &validators),
        Err(PreparationError::Execution(
            ExecutionError::MissingAccountSignature(alice)
        )),
    );
    let correct = LegalTask::sign_account(payload.clone(), &alice_key).unwrap();
    let mut modified = LegalTask::from_signed_parts(
        LegalTaskPayload::new(
            payload.task_id(),
            1,
            None,
            vec![Operation::Transfer {
                source,
                destination,
                amount: 2,
            }],
        ),
        correct.network_id(),
        [0; 32],
        [0; 64],
        correct.account_signatures().to_vec(),
    );
    assert!(modified.verify(&authorizers).is_err());
    modified.with_account_signature(correct.account_signatures()[0].clone());
    assert!(modified.verify(&authorizers).is_err());
    let verified = correct.verify(&authorizers).unwrap();
    let source = crate::legal_task_codec::encode_legal_task(&correct).unwrap();
    let restored = crate::legal_task_codec::decode_legal_task(&source).unwrap();
    assert_eq!(restored, correct);
    assert_eq!(
        restored.verify(&authorizers).unwrap().request_digest(),
        verified.request_digest()
    );
    assert_eq!(
        book.prepare(&mut state, &verified, 1, &validators),
        Ok(PreparationOutcome::Prepared)
    );
    assert_eq!(
        state.bound_request_digest(verified.task_id()),
        Some(verified.request_digest())
    );
    assert_eq!(state.balance(alice), 1);

    let cross_network = LegalTask::sign_account_in_network(payload, &alice_key, [8; 32]).unwrap();
    assert_eq!(
        cross_network.verify(&authorizers),
        Err(crate::AuthorizationError::WrongNetwork)
    );
    store.remove_files().unwrap();
    let _ = std::fs::remove_file(base.with_extension("lock"));
}
