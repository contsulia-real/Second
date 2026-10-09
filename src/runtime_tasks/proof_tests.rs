use super::*;
use crate::prepared::tests::{key, temp_store, validator_set};
use crate::{
    AuthorizerSet, BftPhase, BftQuorumCertificate, BftStatement, BftTimeoutConfig, BftValue,
    BftVote, FinalityCertificate, FinalityStatement, LegalTaskPayload, Operation, SecondState,
    TaskId, ValidatorVote,
};
use std::time::Duration;

#[tokio::test]
async fn verified_unknown_task_proofs_request_source_without_granting_signing_rights() {
    let validators = validator_set();
    let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
    let task_id = TaskId::parse("unknown-certified-candidate").unwrap();
    let task = crate::test_helpers::sign(
        LegalTaskPayload::new(
            task_id.clone(),
            1,
            None,
            vec![
                Operation::RegisterAccount {
                    account: crate::test_helpers::account(119),
                },
                Operation::Issue {
                    account: crate::test_helpers::account(119),
                    count: 1,
                },
            ],
        ),
        &key(9),
    )
    .unwrap()
    .verify(&authorizers)
    .unwrap();
    let (store, base) = temp_store();
    let mut state = SecondState::genesis([], 1);
    store.initialize(&state, &validators).unwrap();
    let allocation = crate::CurrencyAllocation::new(&task, validators.version(), 1).unwrap();
    let allocation_statement = allocation.finality_statement();
    let allocation_certificate = FinalityCertificate::new(
        allocation_statement,
        (2..=4)
            .map(|id| {
                ValidatorVote::sign_unchecked(
                    &allocation_statement,
                    ValidatorId::new(id),
                    &key((id * 3 + 1) as u8),
                )
            })
            .collect(),
        &validators,
    )
    .unwrap();
    store
        .install_currency_allocation(&allocation, &allocation_certificate)
        .unwrap();
    state = store.load().unwrap().unwrap().state;
    PreparedTaskBook::new(store.clone())
        .unwrap()
        .prepare(&mut state, &task, 1, &validators)
        .unwrap();
    let bind_runtime = |store: &crate::StateStore| {
        NodeRuntime::bind_loaded(
            "127.0.0.1:0".parse().unwrap(),
            store,
            store.load().unwrap().unwrap(),
            crate::NodeRuntimeCapabilities::default().with_validator(
                crate::ValidatorRuntimeKeys::new(ValidatorId::new(1), key(3), key(4)),
                crate::ValidatorRuntimeConfig::new(
                    authorizers.clone(),
                    BftTimeoutConfig::new(
                        Duration::from_secs(1),
                        Duration::from_secs(1),
                        Duration::from_secs(1),
                    ),
                    || 1,
                ),
            ),
        )
        .unwrap()
    };
    let runtime = bind_runtime(&store);
    let bft = runtime.validator_bft.as_ref().unwrap();
    let scope = ConsensusScope::PreparedTask(task_id.clone());
    let digest = [77; 32];
    assert_ne!(
        store
            .prepared_bft_proposal_subject(task_id.clone())
            .unwrap()
            .digest(),
        digest
    );
    let generation = store.load().unwrap().unwrap().generation;
    // Signature-valid evidence for an unknown digest only starts a bounded pull;
    // business/source validation must still happen before it becomes a candidate.
    for phase in [BftPhase::Prevote, BftPhase::Precommit] {
        let statement = BftStatement::new(1, scope.clone(), 0, phase, BftValue::Digest(digest));
        let votes = (2..=4)
            .map(|id| {
                BftVote::sign_unchecked(&statement, ValidatorId::new(id), &key((id * 3 + 1) as u8))
            })
            .collect::<Vec<_>>();
        let forged = BftQuorumCertificate::from_untrusted_parts(
            statement.clone(),
            vec![BftVote::from_untrusted_parts(ValidatorId::new(2), [0; 64])],
        );
        let inbound = |certificate| InboundBftMessage {
            validator_id: ValidatorId::new(2),
            message: BftNetworkMessage::QuorumCertificate(certificate),
        };
        assert!(
            runtime
                .process_prepared_task_sync(vec![inbound(forged)])
                .unwrap()
                .is_empty()
        );
        assert!(!bft.has_pending_prepared_task_sync());
        let qc = BftQuorumCertificate::new(statement, votes, &validators).unwrap();
        assert_eq!(
            runtime
                .process_prepared_task_sync(vec![inbound(qc)])
                .unwrap()
                .len(),
            1
        );
        assert!(bft.has_pending_prepared_task_sync());
        bft.finish_prepared_task_fetch(&scope, digest);
    }
    let statement = FinalityStatement::new(1, 1, digest);
    let certificate = FinalityCertificate::new(
        statement,
        (2..=4)
            .map(|id| {
                ValidatorVote::sign_unchecked(
                    &statement,
                    ValidatorId::new(id),
                    &key((id * 3 + 1) as u8),
                )
            })
            .collect(),
        &validators,
    )
    .unwrap();
    assert_eq!(
        runtime
            .process_prepared_task_sync(vec![InboundBftMessage {
                validator_id: ValidatorId::new(2),
                message: BftNetworkMessage::FinalityCertificate {
                    scope: scope.clone(),
                    certificate
                },
            }])
            .unwrap()
            .len(),
        1
    );
    assert!(bft.has_pending_prepared_task_sync());
    assert_eq!(store.load().unwrap().unwrap().generation, generation);
    assert!(
        store
            .validator_set_for_prepared_task(&task_id, digest)
            .is_err()
    );
    // Completion removes resource obligations, but a live provider must still
    // serve the exact certified source to a validator that missed preparation.
    let prepared = store
        .load_prepared_tasks()
        .unwrap()
        .remove(&task_id)
        .unwrap();
    let actual_digest = prepared.plan_digest().unwrap();
    let expected_source = prepared.encode_source().unwrap();
    let statement = FinalityStatement::new(1, 1, actual_digest);
    let certificate = FinalityCertificate::new(
        statement,
        (2..=4)
            .map(|id| {
                ValidatorVote::sign_unchecked(
                    &statement,
                    ValidatorId::new(id),
                    &key((id * 3 + 1) as u8),
                )
            })
            .collect(),
        &validators,
    )
    .unwrap();
    runtime
        .start_prepared_task_consensus(task_id.clone())
        .unwrap();
    let output = bft.consensus().drive(
        vec![InboundBftMessage {
            validator_id: ValidatorId::new(2),
            message: BftNetworkMessage::FinalityCertificate {
                scope: scope.clone(),
                certificate,
            },
        }],
        tokio::time::Instant::now(),
    );
    assert!(output.business_state_changed);
    assert_eq!(
        store
            .load()
            .unwrap()
            .unwrap()
            .state
            .task_succeeded(task_id.clone()),
        Some(true)
    );
    assert!(!store.load_prepared_tasks().unwrap().contains_key(&task_id));
    let cold_store = crate::StateStore::new(&base);
    let cold = cold_store.load_shared().unwrap().unwrap();
    let receipt = &cold.task_receipts[&task_id];
    assert_eq!(*receipt.source().unwrap(), expected_source);
    assert_eq!(
        receipt.certificate().unwrap().statement().subject_digest(),
        actual_digest
    );
    assert_eq!(
        receipt.allocation().unwrap(),
        &(allocation.start(), allocation_certificate)
    );
    assert!(!receipt.plan().commit_authorized);
    let shared = crate::StateRecoveryPayload::from_persisted(&cold)
        .unwrap()
        .encode_bytes()
        .unwrap();
    let mut without_receipts = (*cold).clone();
    without_receipts.task_receipts.clear();
    assert_eq!(
        shared,
        crate::StateRecoveryPayload::from_persisted(&without_receipts)
            .unwrap()
            .encode_bytes()
            .unwrap()
    );
    drop(runtime);
    let recovered_runtime = bind_runtime(&cold_store);
    recovered_runtime
        .start_durable_prepared_consensus()
        .unwrap();
    let bft = recovered_runtime.validator_bft.as_ref().unwrap();
    let relay = bft.consensus().drive(
        vec![InboundBftMessage {
            validator_id: ValidatorId::new(2),
            message: BftNetworkMessage::Vote {
                statement: BftStatement::new(1, scope.clone(), 0, BftPhase::Prevote, BftValue::Nil),
                vote: BftVote::from_untrusted_parts(ValidatorId::new(2), [0; 64]),
            },
        }],
        tokio::time::Instant::now(),
    );
    assert!(relay.outbound.iter().any(|message| matches!(message, BftNetworkMessage::FinalityCertificate { certificate, .. } if certificate.statement().subject_digest() == actual_digest)));
    drop(recovered_runtime);
    store.remove_files().unwrap();
    let _ = std::fs::remove_file(base.with_extension("lock"));
}
