use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use second::{
    AccountAddress, AuthorizerSet, CURRENT_PROTOCOL_VERSION, CurrencyAddress, CurrencyRole,
    LegalTask, LegalTaskPayload, MAX_PUBLIC_CURRENCY_PAGE, NetworkError, NodeId, Operation,
    SecondState, TaskId, client_public_currency_page, serve_public_currency_session,
};

fn key(byte: u8) -> SigningKey {
    SigningKey::from_bytes(&[byte; 32])
}

fn verified_task(task_id: u128, operations: Vec<Operation>) -> second::VerifiedLegalTask {
    let signing = key(9);
    let authorizers = AuthorizerSet::new(
        CURRENT_PROTOCOL_VERSION,
        [signing.verifying_key().to_bytes()],
    )
    .unwrap();
    LegalTask::sign(
        LegalTaskPayload::new(
            TaskId::new(task_id),
            CURRENT_PROTOCOL_VERSION,
            None,
            operations,
        ),
        &signing,
    )
    .unwrap()
    .verify(&authorizers)
    .unwrap()
}

#[test]
fn real_tcp_query_returns_public_occupancy_and_role_without_owner() {
    let alice = AccountAddress::new(987654);
    let mut state = SecondState::genesis([alice], 1).with_reserve(2).unwrap();
    let issue = verified_task(
        1,
        vec![Operation::Issue {
            account: alice,
            count: 2,
        }],
    );
    state.execute(&issue, 1).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();

        serve_public_currency_session(&mut stream, NodeId::from_u64(1), &state).unwrap()
    });

    let mut client = TcpStream::connect(address).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    client
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();

    let page =
        client_public_currency_page(&mut client, NodeId::from_u64(2), CurrencyAddress::new(1), 4)
            .unwrap();

    assert_eq!(page.remote_node_id, NodeId::from_u64(1));
    assert_eq!(page.states.len(), 4);
    assert_eq!(page.states[0].role, CurrencyRole::Reserve);
    assert!(!page.states[0].occupied);
    assert_eq!(page.states[2].role, CurrencyRole::Circulation);
    assert!(page.states[2].occupied);
    assert_eq!(page.next_start, None);

    let rendered = format!("{:?}", page.states);
    assert!(!rendered.contains("AccountAddress"));
    assert!(!rendered.contains("987654"));

    server.join().unwrap();
}

#[test]
fn address_gaps_are_skipped_without_empty_pages() {
    let alice = AccountAddress::new(1);
    let bob = AccountAddress::new(2);
    let mut state = SecondState::genesis([alice, bob], 1);

    let failing = verified_task(
        1,
        vec![
            Operation::Issue {
                account: alice,
                count: 2,
            },
            Operation::Transfer {
                source: bob,
                destination: alice,
                amount: 1,
            },
        ],
    );
    assert!(state.execute(&failing, 1).is_err());

    let issue = verified_task(
        2,
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );
    state.execute(&issue, 2).unwrap();

    let page = state
        .public_currency_page(CurrencyAddress::new(1), 1)
        .unwrap();
    assert_eq!(page.states.len(), 1);
    assert_eq!(page.states[0].address, CurrencyAddress::new(3));
    assert_eq!(page.next_start, None);
}

#[test]
fn public_currency_page_limit_counts_existing_currencies_not_empty_addresses() {
    let alice = AccountAddress::new(1);
    let bob = AccountAddress::new(2);
    let mut state = SecondState::genesis([alice, bob], 1);

    let failing = verified_task(
        10,
        vec![
            Operation::Issue {
                account: alice,
                count: 10_000,
            },
            Operation::Transfer {
                source: bob,
                destination: alice,
                amount: 1,
            },
        ],
    );
    assert!(state.execute(&failing, 1).is_err());

    let issue = verified_task(
        11,
        vec![Operation::Issue {
            account: alice,
            count: 2,
        }],
    );
    state.execute(&issue, 2).unwrap();

    let page = state
        .public_currency_page(CurrencyAddress::new(1), 2)
        .unwrap();

    assert_eq!(page.states.len(), 2);
    assert_eq!(page.states[0].address, CurrencyAddress::new(10_001));
    assert_eq!(page.states[1].address, CurrencyAddress::new(10_002));
    assert_eq!(page.next_start, None);
}

#[test]
fn public_currency_page_rejects_zero_or_excessive_limit() {
    let state = SecondState::genesis([], 1);

    assert_eq!(
        state.public_currency_page(CurrencyAddress::new(1), 0),
        Err(NetworkError::InvalidPublicCurrencyLimit {
            requested: 0,
            maximum: MAX_PUBLIC_CURRENCY_PAGE,
        })
    );

    assert_eq!(
        state.public_currency_page(CurrencyAddress::new(1), MAX_PUBLIC_CURRENCY_PAGE + 1,),
        Err(NetworkError::InvalidPublicCurrencyLimit {
            requested: MAX_PUBLIC_CURRENCY_PAGE + 1,
            maximum: MAX_PUBLIC_CURRENCY_PAGE,
        })
    );
}
