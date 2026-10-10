use super::projection::push_bounded;
use super::*;
use crate::network::session::{PublicNetworkServices, serve_public_network_connection};
use crate::{AddressRange, CurrencyAddress};
use crate::{CurrencyRole, QuicClient, QuicServer, QuicTransportIdentity, SecondState, StateStore};

static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Fixture {
    store: StateStore,
    loads: Arc<AtomicUsize>,
}
impl Fixture {
    fn new(count: u64) -> Self {
        let account = crate::test_helpers::account(7);
        let mut state = SecondState::genesis([account], 1)
            .with_reserve(count * 2)
            .unwrap();
        // Separate owned runs retain the pagination and range-budget coverage.
        for value in (1..count * 2).step_by(2) {
            let address = CurrencyAddress::new(value);
            state.business.currencies.set_range(
                AddressRange::new(address, 1).unwrap(),
                CurrencyRole::Circulation,
                Some(account),
            );
        }
        Self::from_state(state)
    }
    fn from_state(state: SecondState) -> Self {
        let (store, _) = crate::prepared::tests::temp_store();
        store
            .initialize(&state, &crate::prepared::tests::validator_set())
            .unwrap();
        Self {
            store,
            loads: Arc::new(AtomicUsize::new(0)),
        }
    }
    fn loader(&self) -> SnapshotLoader {
        let store = self.store.clone();
        let loads = Arc::clone(&self.loads);
        Arc::new(move || {
            loads.fetch_add(1, Ordering::Relaxed);
            store
                .load_shared()
                .map_err(|e| NetworkError::Transport(format!("{e:?}")))?
                .ok_or_else(|| denied("unavailable"))
        })
    }
}

#[tokio::test]
async fn billion_owned_units_use_one_asset_row_and_the_same_balance() {
    let _serial = TEST_LOCK.lock().await;
    let account = crate::test_helpers::account(7);
    let mut state = SecondState::genesis([account], 1);
    state.business.currencies.set_range(
        AddressRange::new(CurrencyAddress::new(1), 1_000_000_000).unwrap(),
        CurrencyRole::Circulation,
        Some(account),
    );
    state.protocol.next_currency_address = 1_000_000_001;
    let fixture = Fixture::from_state(state);
    for kind in [1, 4] {
        let (_client, peer, worker) = connection(&fixture, ACCOUNT_QUERY_LIFETIME, true).await;
        let view = client_account_view(&peer, &crate::test_helpers::key(7), kind)
            .await
            .unwrap();
        assert_eq!(view.balance, 1_000_000_000);
        if kind == 4 {
            assert_eq!(view.total, 1);
            assert_eq!(
                view.currencies,
                vec![AccountCurrencyRange {
                    start: CurrencyAddress::new(1).to_string(),
                    len: 1_000_000_000
                }]
            );
        }
        worker.await.unwrap().unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.store.remove_files().unwrap();
        std::fs::remove_file(self.store.base_path()).unwrap();
    }
}

async fn connection(
    fixture: &Fixture,
    lifetime: Duration,
    public_route: bool,
) -> (
    QuicClient,
    QuicPeer,
    tokio::task::JoinHandle<Result<NodeId, NetworkError>>,
) {
    let identity = QuicTransportIdentity::generate().unwrap();
    let server = QuicServer::bind("127.0.0.1:0".parse().unwrap(), &identity).unwrap();
    let address = server.local_addr().unwrap();
    let client = QuicClient::new(
        "127.0.0.1:0".parse().unwrap(),
        identity.certificate_der(),
        QuicTransportIdentity::generate().unwrap(),
    )
    .unwrap();
    let loader = fixture.loader();
    let services = PublicNetworkServices::new(
        crate::network::peer_store::PeerStore::load(
            fixture.store.base_path().with_extension("peers"),
        )
        .unwrap(),
        server.node_id(),
        None,
        crate::network::recovery::new_state_recovery_provider_handle(),
        Arc::new(|_| panic!("private query must not invoke public snapshot loader")),
        Arc::new(|_| panic!("private query must not invoke transition loader")),
        Some(loader.clone()),
    );
    let worker = tokio::spawn(async move {
        let peer = server.accept().await?;
        let result = if public_route {
            serve_public_network_connection(&peer, services).await
        } else {
            let first = peer.accept_request().await?.unwrap();
            serve_until(&peer, first, Some(loader), Instant::now() + lifetime).await
        };
        peer.close();
        result
    });
    let peer = client.connect(address).await.unwrap();
    (client, peer, worker)
}

fn request(peer: &QuicPeer, kind: u8, cursor: u64, generation: Option<u64>) -> NetworkMessage {
    use ed25519_dalek::Signer;
    let key = crate::test_helpers::key(7);
    let mut message = NetworkMessage::AccountQuery {
        account: key.verifying_key().to_bytes(),
        kind,
        cursor,
        generation,
        nonce: [3; 32],
        signature: [0; 64],
    };
    let signed = key
        .sign(&signing_bytes(peer.channel_binding().unwrap(), &message).unwrap())
        .to_bytes();
    if let NetworkMessage::AccountQuery { signature, .. } = &mut message {
        *signature = signed;
    }
    message
}

#[tokio::test]
async fn public_dispatch_authenticates_before_loading_and_rejects_new_connection_continuations() {
    let _serial = TEST_LOCK.lock().await;
    let fixture = Fixture::new(1);
    for invalid_signature in [true, false] {
        let (_client, peer, worker) = connection(&fixture, ACCOUNT_QUERY_LIFETIME, true).await;
        let mut message = request(
            &peer,
            4,
            if invalid_signature { 0 } else { 128 },
            if invalid_signature { None } else { Some(1) },
        );
        if invalid_signature && let NetworkMessage::AccountQuery { signature, .. } = &mut message {
            *signature = [0; 64];
        }
        assert!(matches!(
            peer.exchange(&message).await.unwrap(),
            NetworkMessage::AccountQueryDenied { .. }
        ));
        worker.await.unwrap().unwrap();
        assert_eq!(fixture.loads.load(Ordering::Relaxed), 0);
    }
    // Directly exercise malformed server requests, which the wire decoder rejects even earlier.
    for (kind, cursor, generation) in [
        (0, 0, None),
        (4, 1, Some(1)),
        (1, 128, Some(1)),
        (4, 0, Some(1)),
    ] {
        let message = NetworkMessage::AccountQuery {
            account: [7; 32],
            kind,
            cursor,
            generation,
            nonce: [3; 32],
            signature: [0; 64],
        };
        assert!(
            Projection::build(
                [0; 32],
                &message,
                Some(fixture.loader()),
                Instant::now() + ACCOUNT_QUERY_LIFETIME,
                &AtomicBool::new(false)
            )
            .is_err()
        );
        assert_eq!(fixture.loads.load(Ordering::Relaxed), 0);
    }
    assert_eq!(ACTIVE_PROJECTIONS.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn large_query_loads_once_releases_snapshot_and_survives_durable_metadata_writes() {
    let _serial = TEST_LOCK.lock().await;
    let fixture = Fixture::new(513);
    let initial = fixture.store.load_shared().unwrap().unwrap();
    let old_snapshot = Arc::downgrade(&initial);
    drop(initial);
    let (_client, peer, worker) = connection(&fixture, ACCOUNT_QUERY_LIFETIME, true).await;
    let key = crate::test_helpers::key(7);
    let mut view = client_account_query(&peer, &key, 4, 0, None).await.unwrap();
    let generation = view.generation;
    fixture.store.attach_checkpoint_proof(None).unwrap();
    assert!(fixture.store.load_shared().unwrap().unwrap().generation > generation);
    assert!(
        old_snapshot.upgrade().is_none(),
        "projection must not retain the full private snapshot"
    );
    while let Some(cursor) = view.next {
        fixture.store.attach_checkpoint_proof(None).unwrap();
        let page = client_account_query(&peer, &key, 4, cursor, Some(generation))
            .await
            .unwrap();
        super::super::client::append_page(&mut view, page).unwrap();
    }
    assert_eq!(view.currencies.len(), 513);
    assert_eq!(view.balance, 513);
    assert_eq!(
        fixture.loads.load(Ordering::Relaxed),
        1,
        "continuations cannot reload or rescan Currency"
    );
    worker.await.unwrap().unwrap();
    assert_eq!(ACTIVE_PROJECTIONS.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn disconnect_and_concurrency_budget_release_all_projections_without_loading_when_busy() {
    let _serial = TEST_LOCK.lock().await;
    let fixture = Fixture::new(129);
    let mut sessions = Vec::new();
    for _ in 0..MAX_ACTIVE_PROJECTIONS {
        let (client, peer, worker) = connection(&fixture, ACCOUNT_QUERY_LIFETIME, false).await;
        client_account_query(&peer, &crate::test_helpers::key(7), 4, 0, None)
            .await
            .unwrap();
        sessions.push((client, peer, worker));
    }
    let (_client, peer, worker) = connection(&fixture, ACCOUNT_QUERY_LIFETIME, false).await;
    assert!(
        matches!(client_account_query(&peer, &crate::test_helpers::key(7), 4, 0, None).await,
        Err(NetworkError::AccountQueryDenied(reason)) if reason == "busy")
    );
    worker.await.unwrap().unwrap();
    assert_eq!(
        fixture.loads.load(Ordering::Relaxed),
        MAX_ACTIVE_PROJECTIONS
    );
    for (_, peer, worker) in sessions {
        peer.close();
        worker.await.unwrap().unwrap();
    }
    assert_eq!(ACTIVE_PROJECTIONS.load(Ordering::Relaxed), 0);
    let (_client, peer, worker) = connection(&fixture, ACCOUNT_QUERY_LIFETIME, false).await;
    assert_eq!(
        client_account_view(&peer, &crate::test_helpers::key(7), 4)
            .await
            .unwrap()
            .currencies
            .len(),
        129
    );
    worker.await.unwrap().unwrap();
    assert_eq!(ACTIVE_PROJECTIONS.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn idle_and_absolute_expiration_release_results_even_with_quic_keepalive() {
    let _serial = TEST_LOCK.lock().await;
    let fixture = Fixture::new(129);
    for lifetime in [Duration::from_secs(1), ACCOUNT_QUERY_LIFETIME] {
        let (_client, peer, worker) = connection(&fixture, lifetime, false).await;
        client_account_query(&peer, &crate::test_helpers::key(7), 4, 0, None)
            .await
            .unwrap();
        let expired = tokio::time::timeout(Duration::from_secs(7), worker)
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(expired, Err(NetworkError::AccountQueryDenied(reason)) if reason == "session_expired")
        );
        assert_eq!(ACTIVE_PROJECTIONS.load(Ordering::Relaxed), 0);
        assert!(
            client_account_query(&peer, &crate::test_helpers::key(7), 4, 128, Some(1))
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn oversized_assets_fail_without_partial_results_but_balance_remains_available() {
    let _serial = TEST_LOCK.lock().await;
    let fixture = Fixture::new(MAX_ACCOUNT_QUERY_ROWS as u64 + 1);
    let (_client, peer, worker) = connection(&fixture, ACCOUNT_QUERY_LIFETIME, false).await;
    assert!(
        matches!(client_account_view(&peer, &crate::test_helpers::key(7), 4).await,
        Err(NetworkError::AccountQueryDenied(reason)) if reason == "budget_exceeded")
    );
    worker.await.unwrap().unwrap();
    assert_eq!(ACTIVE_PROJECTIONS.load(Ordering::Relaxed), 0);
    let (_client, peer, worker) = connection(&fixture, ACCOUNT_QUERY_LIFETIME, false).await;
    let view = client_account_view(&peer, &crate::test_helpers::key(7), 1)
        .await
        .unwrap();
    assert_eq!(view.balance, MAX_ACCOUNT_QUERY_ROWS as u64 + 1);
    assert!(view.currencies.is_empty());
    worker.await.unwrap().unwrap();
    let mut rows = Vec::<CurrencyAddress>::new();
    assert!(push_bounded(&mut rows, CurrencyAddress::new(1), MAX_PROJECTION_BYTES).is_err());
    assert!(rows.is_empty());
}
