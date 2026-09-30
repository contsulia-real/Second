use crate::support;
use support::FinalizedExecute as _;

use second::{
    CurrencyAddress, CurrencyRole, MAX_PUBLIC_CURRENCY_PAGE, NetworkError, NodeId, Operation,
    SecondState, client_public_currency_page, serve_public_currency_connection,
};
use support::{payment_address, register_payment_addresses, verified_task};

#[tokio::test]
async fn real_quic_query_returns_public_occupancy_and_role_without_owner() {
    let alice = support::account(987654);
    let mut state = SecondState::genesis([alice], 1).with_reserve(2).unwrap();
    let issue = verified_task(
        1,
        vec![Operation::Issue {
            account: alice,
            count: 2,
        }],
    );
    state.execute_finalized(&issue, 1).unwrap();

    let (server, certificate) = support::quic_server();
    let address = server.local_addr().unwrap();

    let server_task = tokio::spawn(async move {
        let peer = server.accept(NodeId::from_u64(1)).await.unwrap();
        serve_public_currency_connection(&peer, &state, None)
            .await
            .unwrap()
    });

    let client = support::quic_client(&certificate);
    let peer = client.connect(address, NodeId::from_u64(2)).await.unwrap();

    let page = client_public_currency_page(&peer, CurrencyAddress::new(1), 4)
        .await
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

    peer.close();
    assert_eq!(server_task.await.unwrap(), NodeId::from_u64(2));
}

#[test]
fn address_gaps_are_skipped_without_empty_pages() {
    let alice = support::account(1);
    let bob = support::account(2);
    let mut state = SecondState::genesis([alice, bob], 1);
    register_payment_addresses(&mut state, [alice, bob]);

    let failing = verified_task(
        1,
        vec![
            Operation::Issue {
                account: alice,
                count: 2,
            },
            Operation::Transfer {
                source: payment_address(bob),
                destination: payment_address(alice),
                amount: 1,
            },
        ],
    );
    assert!(state.execute_finalized(&failing, 1).is_err());

    let issue = verified_task(
        2,
        vec![Operation::Issue {
            account: alice,
            count: 1,
        }],
    );
    state.execute_finalized(&issue, 2).unwrap();

    let page = state
        .public_currency_page(CurrencyAddress::new(1), 1)
        .unwrap();
    assert_eq!(page.states.len(), 1);
    assert_eq!(page.states[0].address, CurrencyAddress::new(3));
    assert_eq!(page.next_start, None);
}

#[test]
fn public_currency_page_limit_counts_existing_currencies_not_empty_addresses() {
    let alice = support::account(1);
    let bob = support::account(2);
    let mut state = SecondState::genesis([alice, bob], 1);
    register_payment_addresses(&mut state, [alice, bob]);

    let failing = verified_task(
        10,
        vec![
            Operation::Issue {
                account: alice,
                count: 10_000,
            },
            Operation::Transfer {
                source: payment_address(bob),
                destination: payment_address(alice),
                amount: 1,
            },
        ],
    );
    assert!(state.execute_finalized(&failing, 1).is_err());

    let issue = verified_task(
        11,
        vec![Operation::Issue {
            account: alice,
            count: 2,
        }],
    );
    state.execute_finalized(&issue, 2).unwrap();

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
