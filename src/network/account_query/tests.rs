use super::client::{append_page, validate_page};
use super::*;
use crate::{AccountAddress, CurrencyAddress};

fn page(cursor: u64, total: u64) -> AccountView {
    let end = (cursor + u64::from(MAX_ACCOUNT_QUERY_PAGE)).min(total);
    AccountView {
        account: AccountAddress::from_bytes([7; 32]).to_string(),
        kind: 4,
        cursor,
        generation: 1,
        validator_set_version: 1,
        exists: true,
        balance: total,
        total,
        addresses: Vec::new(),
        transfers: Vec::new(),
        currencies: (cursor..end)
            .map(|i| CurrencyAddress::new(i + 1).to_string())
            .collect(),
        next: (end < total).then_some(end),
        nonce: vec![1; 32],
    }
}

#[test]
fn pages_reject_omission_duplicates_wrong_shape_and_invalid_boundaries() {
    let first = page(0, 129);
    validate_page(&first).unwrap();
    let corruptions: &[fn(&mut AccountView)] = &[
        |p| p.currencies[1] = p.currencies[0].clone(),
        |p| p.currencies.swap(0, 1),
        |p| p.currencies[0] = "invalid".into(),
        |p| p.next = None,
        |p| p.next = Some(127),
        |p| p.cursor = 1,
        |p| p.currencies.clear(),
        |p| p.kind = 1,
        |p| p.balance += 1,
        |p| p.total = MAX_ACCOUNT_QUERY_ROWS as u64 + 1,
        |p| p.exists = false,
        |p| p.nonce.clear(),
        |p| p.generation = 0,
    ];
    for (index, corrupt) in corruptions.iter().enumerate() {
        let mut changed = first.clone();
        corrupt(&mut changed);
        assert!(validate_page(&changed).is_err(), "corruption {index}");
    }
    let tail = page(128, 129);
    for mutate in [
        (|p: &mut AccountView| p.currencies[0] = CurrencyAddress::new(128).to_string())
            as fn(&mut AccountView),
        |p| p.currencies[0] = CurrencyAddress::new(1).to_string(),
        |p| p.generation += 1,
        |p| p.validator_set_version += 1,
        |p| p.account = AccountAddress::from_bytes([8; 32]).to_string(),
        |p| p.balance += 1,
        |p| p.currencies.clear(),
        |p| p.next = Some(256),
    ] {
        let mut changed = tail.clone();
        mutate(&mut changed);
        assert!(append_page(&mut first.clone(), changed).is_err());
    }
    let mut complete = first;
    append_page(&mut complete, tail).unwrap();
    assert_eq!(complete.currencies.len() as u64, complete.balance);
    assert_eq!(complete.next, None);
    let empty = page(0, 0);
    validate_page(&empty).unwrap();
    let mut balance = empty;
    balance.kind = 1;
    balance.balance = u64::MAX;
    validate_page(&balance).unwrap();
}

#[test]
fn account_codec_rejects_invalid_requests_and_roundtrips_failure_reasons() {
    let query = NetworkMessage::AccountQuery {
        account: [1; 32],
        kind: 4,
        cursor: 0,
        generation: None,
        nonce: [2; 32],
        signature: [3; 64],
    };
    let frame = crate::encode_network_message(&query).unwrap();
    assert_eq!(crate::decode_network_message(&frame).unwrap(), query);
    for (kind, cursor, generation) in [
        (0, 0, None),
        (5, 0, None),
        (4, 1, Some(1)),
        (4, 128, None),
        (1, 128, Some(1)),
        (4, 0, Some(1)),
        (4, 128, Some(0)),
        (4, 100_096, Some(1)),
    ] {
        assert!(validate_request(kind, cursor, generation).is_err());
    }
    for reason in [
        "unauthorized",
        "unavailable",
        "invalid_query",
        "budget_exceeded",
        "busy",
        "session_expired",
    ] {
        let message = rejected(reason);
        assert_eq!(
            crate::decode_network_message(&crate::encode_network_message(&message).unwrap())
                .unwrap(),
            message
        );
    }
    // Mutate the wire payload rather than relying only on the sender's encoder checks.
    for (offset, value) in [(12 + 33, 5), (12 + 41, 1), (12 + 42, 1)] {
        let mut invalid = frame.clone();
        invalid[offset] = value;
        assert!(crate::decode_network_message(&invalid).is_err());
    }
}

#[tokio::test]
async fn full_client_rejects_cross_page_duplicates_and_message_mixups_over_quic() {
    use crate::{QuicClient, QuicServer, QuicTransportIdentity};
    for variant in 0..3 {
        let identity = QuicTransportIdentity::generate().unwrap();
        let server = QuicServer::bind("127.0.0.1:0".parse().unwrap(), &identity).unwrap();
        let client = QuicClient::new(
            "127.0.0.1:0".parse().unwrap(),
            identity.certificate_der(),
            QuicTransportIdentity::generate().unwrap(),
        )
        .unwrap();
        let address = server.local_addr().unwrap();
        let worker = tokio::spawn(async move {
            let peer = server.accept().await.unwrap();
            for cursor in [0, 128] {
                let request = peer.accept_request().await.unwrap().unwrap();
                let NetworkMessage::AccountQuery { account, nonce, .. } = request.message() else {
                    panic!()
                };
                let mut view = page(cursor, 129);
                view.account = AccountAddress::from_bytes(*account).to_string();
                view.nonce = nonce.to_vec();
                if variant == 2 {
                    request
                        .respond(&NetworkMessage::Pong { nonce: 1 })
                        .await
                        .unwrap();
                    break;
                }
                if variant == 1 {
                    view.next = None;
                }
                if cursor == 128 {
                    view.currencies[0] = CurrencyAddress::new(128).to_string();
                }
                request
                    .respond(&NetworkMessage::AccountQueryResult { view })
                    .await
                    .unwrap();
                if variant == 1 {
                    break;
                }
            }
        });
        let peer = client.connect(address).await.unwrap();
        assert!(
            client_account_view(&peer, &crate::test_helpers::key(7), 4)
                .await
                .is_err()
        );
        peer.close();
        worker.await.unwrap();
    }
}
