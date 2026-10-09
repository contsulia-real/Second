use super::*;
use crate::runtime_bft::tests::{key, temp_store, validator_set};
use crate::{
    AuthorizerSet, BftTimeoutConfig, SecondState, ValidatorRuntimeConfig, ValidatorRuntimeKeys,
};
use std::time::Duration;

#[tokio::test]
async fn source_request_does_not_stop_announcements_to_later_proposers() {
    use crate::runtime::ActiveConnectionPermit;
    use crate::runtime_bft::{ManagedValidatorBftPeer, run_validator_bft_sender};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize},
    };
    use tokio::sync::mpsc;

    let (store, base) = temp_store();
    let validators = validator_set(1);
    let authorizers = AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap();
    let alice = crate::test_helpers::account(231);
    let bob = crate::test_helpers::account(232);
    let source = crate::PaymentAddress::from_bytes([231; 32]);
    let destination = crate::PaymentAddress::from_bytes([232; 32]);
    let mut state = SecondState::genesis([alice, bob], 3);
    for (address, account) in [(source, alice), (destination, bob)] {
        state.business.payment_addresses.insert(
            address,
            crate::payment::PaymentAddressRecord {
                account,
                status: crate::PaymentAddressStatus::Active,
            },
        );
    }
    for value in 1..=2 {
        let address = crate::CurrencyAddress::new(value);
        state.business.currencies.insert(
            address,
            crate::currency::Currency {
                address,
                role: crate::CurrencyRole::Circulation,
                owner: Some(alice),
            },
        );
    }
    store.initialize(&state, &validators).unwrap();
    let task = crate::test_helpers::sign(
        crate::LegalTaskPayload::new(
            crate::TaskId::parse("source-proposer-change").unwrap(),
            1,
            None,
            vec![crate::Operation::Transfer {
                source,
                destination,
                amount: 1,
            }],
        ),
        &key(9),
    )
    .unwrap()
    .verify(&authorizers)
    .unwrap();
    let mut book = crate::PreparedTaskBook::new(store.clone()).unwrap();
    book.prepare(&mut state, &task, 1, &validators).unwrap();
    let mut second = store.load_shared().unwrap().unwrap().prepared_tasks[&task.task_id()].clone();
    let crate::prepared_plan::PreparedOperation::Transfer { currencies, .. } =
        &mut second.operations[0]
    else {
        panic!("transfer fixture")
    };
    *currencies = vec![crate::CurrencyAddress::new(2)];
    assert!(
        book.admit_frozen_variant(
            &mut state,
            &task,
            &validators,
            second.plan_digest().unwrap(),
            &[vec![crate::CurrencyAddress::new(2)]]
        )
        .unwrap()
        .is_none()
    );
    let expected = store.load_shared().unwrap().unwrap().prepared_tasks[&task.task_id()]
        .owned_candidate_digests()
        .unwrap()
        .into_iter()
        .collect::<BTreeSet<_>>();
    assert_eq!(expected.len(), 2);
    let subject = store.prepared_bft_proposal_subject(task.task_id()).unwrap();
    let runtime = ValidatorBftRuntime::new(
        ValidatorRuntimeKeys::new(ValidatorId::new(2), key(6), key(7)),
        ValidatorRuntimeConfig::new(
            authorizers,
            BftTimeoutConfig::new(
                Duration::from_secs(1),
                Duration::from_secs(1),
                Duration::from_secs(1),
            ),
            || 1,
        ),
        store.clone(),
        validators.clone(),
        std::iter::empty(),
    )
    .unwrap();
    let identity = crate::QuicTransportIdentity::generate().unwrap();
    let server = crate::QuicServer::bind("127.0.0.1:0".parse().unwrap(), &identity).unwrap();
    let client = crate::QuicClient::new(
        "127.0.0.1:0".parse().unwrap(),
        identity.certificate_der(),
        crate::QuicTransportIdentity::generate().unwrap(),
    )
    .unwrap();
    let address = server.local_addr().unwrap();
    let (arrived, mut arrivals) = mpsc::channel(4);
    let serving_set = validators.clone();
    let serving = tokio::spawn(async move {
        let peer = server.accept().await.unwrap();
        crate::serve_validator_bft_connection(
            &peer,
            ValidatorId::new(3),
            &key(9),
            &serving_set,
            |sender, message| {
                assert_eq!(sender, ValidatorId::new(2));
                arrived.try_send(message).unwrap();
                std::future::ready(Ok(()))
            },
        )
        .await
    });
    let peer = crate::network::authenticate_validator_bft_peer_with_authority(
        client.connect(address).await.unwrap(),
        ValidatorId::new(2),
        &key(6),
        &runtime.inner.authority,
    )
    .await
    .unwrap();
    let (sender, normal) = mpsc::channel(4);
    let (finality_sender, finality) = mpsc::channel(4);
    let alive = Arc::new(AtomicBool::new(true));
    let active = Arc::new(AtomicUsize::new(0));
    runtime.inner.outbound.lock().unwrap().insert(
        ValidatorId::new(3),
        ManagedValidatorBftPeer {
            peer: peer.clone(),
            sender,
            finality_sender,
            alive: Arc::clone(&alive),
            _client: client,
            _permit: ActiveConnectionPermit::try_acquire(&active).unwrap(),
        },
    );
    let worker = tokio::spawn(run_validator_bft_sender(
        peer.clone(),
        ValidatorId::new(3),
        normal,
        finality,
        alive,
        Arc::downgrade(&runtime.inner),
    ));

    runtime.announce_prepared_task(&validators, subject.scope().clone(), subject.digest());
    // The first proposer starts a pull, then disappears before publishing a
    // proposal. A request is not evidence that the source was replicated.
    runtime.serve_prepared_task_request(
        ValidatorId::new(1),
        1,
        subject.scope().clone(),
        subject.digest(),
        0,
    );
    store
        .catch_up_bft_round(ValidatorId::new(2), subject.scope(), 1, &validators)
        .unwrap();
    runtime.retry_prepared_task_sync(false);
    assert!(
        runtime.has_pending_prepared_task_sync(),
        "a partial pull retired source availability"
    );
    assert!(matches!(
        arrivals.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    store
        .advance_bft_round(ValidatorId::new(2), subject.scope(), 2)
        .unwrap();
    let generation = store.load_shared().unwrap().unwrap().generation;
    runtime.retry_prepared_task_sync(false);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), arrivals.recv())
            .await
            .unwrap()
            .unwrap(),
        BftNetworkMessage::PreparedTaskAvailable {
            validator_set_version: 1,
            scope: subject.scope().clone(),
            expected_plan_digest: subject.digest(),
            round: 2
        }
    );
    crate::runtime_bft_consensus::start_prepared_task_consensus_for(
        &store,
        &runtime,
        task.task_id(),
    )
    .unwrap();
    let mut observed = BTreeSet::new();
    tokio::time::timeout(Duration::from_secs(3), async {
        while observed != expected {
            let BftNetworkMessage::PreparedTaskAvailable {
                scope,
                validator_set_version,
                expected_plan_digest,
                round,
            } = arrivals.recv().await.unwrap()
            else {
                panic!("unexpected source announcement")
            };
            assert_eq!(scope, *subject.scope());
            assert_eq!((validator_set_version, round), (1, 2));
            assert!(expected.contains(&expected_plan_digest));
            observed.insert(expected_plan_digest);
        }
    })
    .await
    .expect("every owned frozen variant must be announced to the new proposer");
    for (acknowledger, remaining) in [
        (ValidatorId::new(1), expected.clone()),
        (
            ValidatorId::new(3),
            expected
                .iter()
                .copied()
                .filter(|digest| *digest != subject.digest())
                .collect(),
        ),
    ] {
        runtime.acknowledge_prepared_task_announcement(
            acknowledger,
            1,
            subject.scope(),
            subject.digest(),
        );
        runtime.retry_prepared_task_sync(false);
        let mut retried = BTreeSet::new();
        for _ in 0..remaining.len() {
            let BftNetworkMessage::PreparedTaskAvailable {
                expected_plan_digest,
                round,
                ..
            } = tokio::time::timeout(Duration::from_secs(3), arrivals.recv())
                .await
                .unwrap()
                .unwrap()
            else {
                panic!("unexpected retry")
            };
            assert_eq!(round, 2);
            retried.insert(expected_plan_digest);
        }
        assert_eq!(
            retried, remaining,
            "a stale or single-candidate acknowledgement lost another source"
        );
    }
    assert_eq!(store.load_shared().unwrap().unwrap().generation, generation);
    runtime.finish_prepared_task_sync(subject.scope());
    assert!(!runtime.has_pending_prepared_task_sync());
    peer.close();
    worker.abort();
    let _ = worker.await;
    let _ = tokio::time::timeout(Duration::from_secs(3), serving)
        .await
        .unwrap();
    drop(runtime);
    store.remove_files().unwrap();
    let _ = std::fs::remove_file(base.with_extension("lock"));
}

#[test]
fn higher_round_and_late_responses_preserve_exact_fetch_without_accepting_changed_bytes() {
    let (store, _) = temp_store();
    let validators = validator_set(1);
    store
        .initialize(&SecondState::genesis([], 1), &validators)
        .unwrap();
    let runtime = ValidatorBftRuntime::new(
        ValidatorRuntimeKeys::new(ValidatorId::new(1), key(3), key(4)),
        ValidatorRuntimeConfig::new(
            AuthorizerSet::new(1, [key(9).verifying_key().to_bytes()]).unwrap(),
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
    let scope = ConsensusScope::PreparedTask(crate::TaskId::parse("fetch-reordered").unwrap());
    let sender = ValidatorId::new(2);
    runtime.begin_prepared_task_fetch(sender, 1, scope.clone(), [7; 32], 0);
    {
        let mut sync = runtime.inner.task_sync.lock().unwrap();
        let fetch = sync.fetches.get_mut(&(scope.clone(), [7; 32])).unwrap();
        fetch.current_source = Some(sender);
        fetch.total_len = Some(4);
        fetch.bytes = vec![1, 2];
    }
    let generation = store.load().unwrap().unwrap().generation;
    runtime.begin_prepared_task_fetch(sender, 1, scope.clone(), [7; 32], 9);
    runtime.reject_prepared_task_source(&scope, sender, 1, [8; 32]);
    let chunk = |digest, offset, bytes| PreparedTaskChunk {
        sender,
        validator_set_version: 1,
        scope: scope.clone(),
        expected_plan_digest: digest,
        total_len: 4,
        offset,
        bytes,
    };
    assert!(matches!(
        runtime.ingest_prepared_task_chunk(chunk([8; 32], 0, vec![1, 2])),
        PreparedTaskChunkResult::Ignored
    ));
    assert!(matches!(
        runtime.ingest_prepared_task_chunk(chunk([7; 32], 0, vec![1, 2])),
        PreparedTaskChunkResult::Ignored
    ));
    assert!(matches!(
        runtime.ingest_prepared_task_chunk(chunk([7; 32], 0, vec![1, 9])),
        PreparedTaskChunkResult::Reject
    ));
    assert!(matches!(
        runtime.ingest_prepared_task_chunk(chunk([7; 32], 3, vec![4])),
        PreparedTaskChunkResult::Reject
    ));
    {
        let sync = runtime.inner.task_sync.lock().unwrap();
        let fetch = &sync.fetches[&(scope.clone(), [7; 32])];
        assert_eq!(fetch.announced_round, 9);
        assert_eq!(fetch.current_source, Some(sender));
        assert_eq!(fetch.bytes, vec![1, 2]);
    }
    // Another frozen candidate in this same scope must not discard the
    // partially received original, even at a higher announced round.
    runtime.begin_prepared_task_fetch(ValidatorId::new(3), 1, scope.clone(), [8; 32], 10);
    {
        let sync = runtime.inner.task_sync.lock().unwrap();
        assert!(
            sync.fetches.values().any(|fetch| {
                fetch.expected_plan_digest == [7; 32]
                    && fetch.current_source == Some(sender)
                    && fetch.bytes == vec![1, 2]
            }),
            "a distinct frozen candidate discarded an in-flight source"
        );
    }
    match runtime.ingest_prepared_task_chunk(chunk([7; 32], 2, vec![3, 4])) {
        PreparedTaskChunkResult::Complete(bytes) => assert_eq!(bytes, vec![1, 2, 3, 4]),
        _ => panic!("exact remaining bytes must complete the original fetch"),
    }
    runtime.finish_prepared_task_fetch(&scope, [7; 32]);
    assert!(matches!(
        runtime.ingest_prepared_task_chunk(chunk([7; 32], 2, vec![3, 4])),
        PreparedTaskChunkResult::Ignored
    ));
    assert!(runtime.has_pending_prepared_task_sync());
    {
        let mut sync = runtime.inner.task_sync.lock().unwrap();
        let fetch = sync.fetches.get_mut(&(scope.clone(), [8; 32])).unwrap();
        fetch.current_source = Some(ValidatorId::new(3));
    }
    runtime.reject_prepared_task_source(&scope, sender, 1, [7; 32]);
    let mut second_chunk = chunk([8; 32], 0, vec![5, 6, 7, 8]);
    second_chunk.sender = ValidatorId::new(3);
    match runtime.ingest_prepared_task_chunk(second_chunk) {
        PreparedTaskChunkResult::Complete(bytes) => assert_eq!(bytes, vec![5, 6, 7, 8]),
        _ => panic!("completing one candidate discarded another candidate's source"),
    }
    runtime.finish_prepared_task_fetch(&scope, [8; 32]);
    assert!(!runtime.has_pending_prepared_task_sync());
    let allocation = |version, start| ConsensusScope::CurrencyAllocation {
        validator_set_version: version,
        start,
    };
    let old_committee = allocation(1, 8);
    let old_frontier = allocation(2, 7);
    let active_frontier = allocation(2, 8);
    let future_frontier = allocation(2, 9);
    let future_committee = allocation(3, 1);
    let scopes = [
        old_committee.clone(),
        old_frontier.clone(),
        active_frontier.clone(),
        future_frontier.clone(),
        future_committee.clone(),
        scope.clone(),
    ];
    for queued in &scopes {
        let version = match queued {
            ConsensusScope::CurrencyAllocation {
                validator_set_version,
                ..
            } => *validator_set_version,
            _ => 1,
        };
        runtime.begin_prepared_task_fetch(sender, version, queued.clone(), [7; 32], 0);
        runtime
            .inner
            .task_sync
            .lock()
            .unwrap()
            .announcements
            .insert(
                (queued.clone(), [7; 32]),
                PreparedTaskAnnouncement {
                    validator_set_version: version,
                    scope: queued.clone(),
                    expected_plan_digest: [7; 32],
                    round: 0,
                    proposer: sender,
                },
            );
    }
    runtime.retire_superseded_allocation_sync(2, 8);
    {
        let sync = runtime.inner.task_sync.lock().unwrap();
        for retired in [old_committee, old_frontier] {
            assert!(!sync.fetches.contains_key(&(retired.clone(), [7; 32])));
            assert!(!sync.announcements.contains_key(&(retired, [7; 32])));
        }
        for retained in [
            active_frontier,
            future_frontier,
            future_committee,
            scope.clone(),
        ] {
            assert!(sync.fetches.contains_key(&(retained.clone(), [7; 32])));
            assert!(sync.announcements.contains_key(&(retained, [7; 32])));
        }
    }
    for queued in &scopes {
        runtime.finish_prepared_task_sync(queued);
    }
    assert!(!runtime.has_pending_prepared_task_sync());
    for seed in 0..MAX_PENDING_UNREGISTERED_SCOPES {
        runtime.begin_prepared_task_fetch(sender, 1, scope.clone(), [seed as u8; 32], 0);
    }
    runtime.begin_prepared_task_fetch(sender, 1, scope.clone(), [255; 32], 1);
    runtime.begin_prepared_task_fetch(sender, 1, scope.clone(), [7; 32], 11);
    {
        let sync = runtime.inner.task_sync.lock().unwrap();
        assert_eq!(sync.fetches.len(), MAX_PENDING_UNREGISTERED_SCOPES);
        assert!(!sync.fetches.contains_key(&(scope.clone(), [255; 32])));
        assert_eq!(sync.fetches[&(scope.clone(), [7; 32])].announced_round, 11);
    }
    runtime.finish_prepared_task_sync(&scope);
    assert!(!runtime.has_pending_prepared_task_sync());
    assert_eq!(store.load().unwrap().unwrap().generation, generation);
    drop(runtime);
    store.remove_files().unwrap();
}

#[test]
fn expired_silent_source_switches_to_an_untried_authorized_peer() {
    let scope = ConsensusScope::PreparedTask(
        crate::TaskId::from_ascii_bytes(b"source-timeout-regression").unwrap(),
    );
    let mut fetch = PreparedTaskFetch::new(1, scope, [7; 32], 0, ValidatorId::new(2));
    let connected = [ValidatorId::new(2), ValidatorId::new(3)];
    let timeout = std::time::Duration::from_secs(1);
    let now = tokio::time::Instant::now();

    assert_eq!(
        fetch.select_source(&connected, ValidatorId::new(1), false, now, timeout,),
        Some(ValidatorId::new(2))
    );

    assert_eq!(
        fetch.select_source(
            &connected,
            ValidatorId::new(1),
            true,
            now + timeout,
            timeout,
        ),
        Some(ValidatorId::new(3))
    );
    assert_eq!(
        fetch.select_source(
            &connected,
            ValidatorId::new(1),
            false,
            now + timeout * 2,
            timeout
        ),
        None
    );
    assert_eq!(
        fetch.select_source(
            &connected,
            ValidatorId::new(1),
            true,
            now + timeout * 2,
            timeout
        ),
        Some(ValidatorId::new(2)),
        "the scheduled retry must reopen an exhausted source cycle"
    );
}
