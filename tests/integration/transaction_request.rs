use crate::support;

use second::{
    AuthorizerSet, CURRENT_PROTOCOL_VERSION, CurrencyAddress, LegalTask, LegalTaskPayload,
    Operation, TaskId, TransactionRequestParseError, parse_transaction_request_json,
};
use serde_json::json;
use support::{account, key, payment};

#[test]
fn strict_json_request_maps_losslessly_to_the_signed_typed_task() {
    let issuer = key(7);
    let operations = vec![
        Operation::Transfer {
            source: payment(1),
            destination: payment(2),
            amount: 2,
        },
        Operation::Issue {
            account: account(3),
            count: 4,
        },
        Operation::Destroy {
            currencies: vec![CurrencyAddress::new(10)],
        },
        Operation::LeakRepair {
            leaked: vec![CurrencyAddress::new(11)],
        },
    ];
    let payload = LegalTaskPayload::new(
        TaskId::parse("request_1").unwrap(),
        CURRENT_PROTOCOL_VERSION,
        Some(100),
        operations.clone(),
    );
    let signed = LegalTask::sign(payload.clone(), &issuer).unwrap();

    let request = json!({
        "request_id": "request_1",
        "version": CURRENT_PROTOCOL_VERSION,
        "expires_at": 100,
        "operations": [
            {
                "type": "transfer",
                "source": payment(1).to_string(),
                "destination": payment(2).to_string(),
                "amount": 2
            },
            {
                "type": "issue",
                "recipient": account(3).to_string(),
                "amount": 4
            },
            {
                "type": "destroy",
                "currencies": [CurrencyAddress::new(10).to_string()]
            },
            {
                "type": "leak_repair",
                "currencies": [CurrencyAddress::new(11).to_string()]
            }
        ],
        "signature": signed.signature_base64url()
    });

    let parsed = parse_transaction_request_json(
        request.to_string().as_bytes(),
        issuer.verifying_key().to_bytes(),
    )
    .unwrap();

    assert_eq!(parsed.payload(), &payload);
    assert_eq!(parsed.signature_bytes(), signed.signature_bytes());

    let authorizers = AuthorizerSet::new(
        CURRENT_PROTOCOL_VERSION,
        [issuer.verifying_key().to_bytes()],
    )
    .unwrap();
    parsed.verify(&authorizers).unwrap();
}

#[test]
fn json_request_can_omit_expiry() {
    let issuer = key(7);
    let payload = LegalTaskPayload::new(
        TaskId::parse("no_expiry").unwrap(),
        CURRENT_PROTOCOL_VERSION,
        None,
        Vec::new(),
    );
    let signed = LegalTask::sign(payload.clone(), &issuer).unwrap();
    let request = json!({
        "request_id": "no_expiry",
        "version": CURRENT_PROTOCOL_VERSION,
        "operations": [],
        "signature": signed.signature_base64url()
    });

    let parsed = parse_transaction_request_json(
        request.to_string().as_bytes(),
        issuer.verifying_key().to_bytes(),
    )
    .unwrap();

    assert_eq!(parsed.payload(), &payload);
}

#[test]
fn strict_json_rejects_duplicate_unknown_and_wrong_json_types() {
    let issuer = key(7);
    let public_key = issuer.verifying_key().to_bytes();
    let signature = LegalTask::sign(
        LegalTaskPayload::new(
            TaskId::parse("r").unwrap(),
            CURRENT_PROTOCOL_VERSION,
            Some(10),
            Vec::new(),
        ),
        &issuer,
    )
    .unwrap()
    .signature_base64url();

    let invalid_json_requests = [
        format!(
            r#"{{"request_id":"r","request_id":"r","version":1,"expires_at":10,"operations":[],"signature":"{signature}"}}"#
        ),
        format!(
            r#"{{"request_id":"r","version":1,"expires_at":10,"operations":[],"signature":"{signature}","extra":1}}"#
        ),
        format!(
            r#"{{"request_id":"r","version":1,"expires_at":"10","operations":[],"signature":"{signature}"}}"#
        ),
        format!(
            r#"{{"request_id":"r","version":1,"expires_at":10,"operations":[{{"type":"issue","recipient":"{}","amount":1.0}}],"signature":"{signature}"}}"#,
            account(1)
        ),
        format!(
            r#"{{"request_id":"r","version":1,"expires_at":10,"operations":[{{"type":"issue","recipient":"{}","amount":1,"amount":1}}],"signature":"{signature}"}}"#,
            account(1)
        ),
        format!(
            r#"{{"request_id":"r","version":1,"expires_at":10,"operations":[{{"type":"issue","recipient":"{}","amount":1,"extra":0}}],"signature":"{signature}"}}"#,
            account(1)
        ),
    ];

    for request in invalid_json_requests {
        assert_eq!(
            parse_transaction_request_json(request.as_bytes(), public_key),
            Err(TransactionRequestParseError::InvalidJson)
        );
    }
}

#[test]
fn strict_json_rejects_protocol_invalid_values_before_signature_verification() {
    let issuer = key(7);
    let public_key = issuer.verifying_key().to_bytes();
    let valid_signature = LegalTask::sign(
        LegalTaskPayload::new(
            TaskId::parse("r").unwrap(),
            CURRENT_PROTOCOL_VERSION,
            Some(10),
            Vec::new(),
        ),
        &issuer,
    )
    .unwrap()
    .signature_base64url();

    let cases = [
        (
            json!({
                "request_id": "r",
                "version": 2,
                "expires_at": 10,
                "operations": [],
                "signature": valid_signature
            }),
            TransactionRequestParseError::UnsupportedProtocolVersion,
        ),
        (
            json!({
                "request_id": "bad.id",
                "version": 1,
                "expires_at": 10,
                "operations": [],
                "signature": valid_signature
            }),
            TransactionRequestParseError::InvalidRequestId,
        ),
        (
            json!({
                "request_id": "r",
                "version": 1,
                "expires_at": 10,
                "operations": [{
                    "type": "transfer",
                    "source": "pay_bad",
                    "destination": payment(2).to_string(),
                    "amount": 1
                }],
                "signature": valid_signature
            }),
            TransactionRequestParseError::InvalidPaymentAddress,
        ),
        (
            json!({
                "request_id": "r",
                "version": 1,
                "expires_at": 10,
                "operations": [{
                    "type": "destroy",
                    "currencies": ["0lxii00"]
                }],
                "signature": valid_signature
            }),
            TransactionRequestParseError::InvalidCurrencyAddress,
        ),
        (
            json!({
                "request_id": "r",
                "version": 1,
                "expires_at": 10,
                "operations": [{
                    "type": "issue",
                    "recipient": account(1).to_string(),
                    "amount": 0
                }],
                "signature": valid_signature
            }),
            TransactionRequestParseError::InvalidOperation,
        ),
        (
            json!({
                "request_id": "r",
                "version": 1,
                "expires_at": 10,
                "operations": [],
                "signature": format!("{valid_signature}=")
            }),
            TransactionRequestParseError::InvalidSignature,
        ),
    ];

    for (request, expected) in cases {
        assert_eq!(
            parse_transaction_request_json(request.to_string().as_bytes(), public_key),
            Err(expected)
        );
    }
}
