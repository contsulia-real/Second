use crate::support;

use second::{
    AuthorizationError, AuthorizerSet, CURRENT_PROTOCOL_VERSION, LegalTask, LegalTaskPayload,
    Operation,
};
use sha2::{Digest, Sha256};
use support::key as signing_key;

fn payload(amount: u64) -> LegalTaskPayload {
    LegalTaskPayload::new(
        support::task_id(42),
        CURRENT_PROTOCOL_VERSION,
        Some(100),
        vec![Operation::Transfer {
            source: support::payment(1),
            destination: support::payment(2),
            amount,
        }],
    )
}

#[test]
fn canonical_signing_bytes_match_the_deterministic_cbor_protocol_vector() {
    let payload = LegalTaskPayload::new(
        second::TaskId::parse("a").unwrap(),
        CURRENT_PROTOCOL_VERSION,
        Some(10),
        Vec::new(),
    );

    let mut expected = b"Second/LegalTask/v1\0".to_vec();
    expected.extend_from_slice(&[0x82, 0x61, b'a', 0x83, 0x01, 0x0a, 0x80]);

    assert_eq!(payload.canonical_signing_bytes().unwrap(), expected);

    let without_expiry = LegalTaskPayload::new(
        second::TaskId::parse("a").unwrap(),
        CURRENT_PROTOCOL_VERSION,
        None,
        Vec::new(),
    );
    let mut expected_without_expiry = b"Second/LegalTask/v1\0".to_vec();
    expected_without_expiry.extend_from_slice(&[0x82, 0x61, b'a', 0x83, 0x01, 0xf6, 0x80]);
    assert_eq!(
        without_expiry.canonical_signing_bytes().unwrap(),
        expected_without_expiry
    );
}

#[test]
fn signature_text_is_strict_canonical_base64url_without_padding() {
    let key = signing_key(7);
    let authorizers =
        AuthorizerSet::new(CURRENT_PROTOCOL_VERSION, [key.verifying_key().to_bytes()]).unwrap();
    let signed = LegalTask::sign(payload(3), &key).unwrap();
    let encoded = signed.signature_base64url();

    assert_eq!(encoded.len(), 86);
    assert!(!encoded.contains(['=', '+', '/']));

    let reparsed =
        LegalTask::from_signature_base64url(payload(3), signed.authorizer_public_key(), &encoded)
            .unwrap();
    assert_eq!(reparsed.signature_bytes(), signed.signature_bytes());
    reparsed.verify(&authorizers).unwrap();

    assert_eq!(
        LegalTask::from_signature_base64url(
            payload(3),
            signed.authorizer_public_key(),
            &(encoded.clone() + "="),
        ),
        Err(second::SignatureParseError::WrongEncodedLength)
    );

    let mut invalid = encoded;
    invalid.replace_range(0..1, "+");
    assert_eq!(
        LegalTask::from_signature_base64url(payload(3), signed.authorizer_public_key(), &invalid,),
        Err(second::SignatureParseError::InvalidBase64Url)
    );
}

#[test]
fn verified_task_request_digest_binds_the_full_signed_request() {
    let key = signing_key(7);
    let authorizers =
        AuthorizerSet::new(CURRENT_PROTOCOL_VERSION, [key.verifying_key().to_bytes()]).unwrap();

    let signed = LegalTask::sign(payload(3), &key).unwrap();
    let verified = signed.verify(&authorizers).unwrap();

    let mut hasher = Sha256::new();
    hasher.update(b"SECOND_SIGNED_LEGAL_TASK_V1\0");
    hasher.update(signed.authorizer_public_key());
    hasher.update(signed.signature_bytes());
    hasher.update(0_u32.to_be_bytes()); // account signature vector length
    hasher.update(signed.canonical_signing_bytes().unwrap());
    let expected: [u8; 32] = hasher.finalize().into();

    assert_eq!(verified.request_digest(), expected);
    assert_eq!(verified.signed_task(), &signed);
}

#[test]
fn changing_any_signed_operation_field_invalidates_the_signature() {
    let key = signing_key(7);
    let authorizers =
        AuthorizerSet::new(CURRENT_PROTOCOL_VERSION, [key.verifying_key().to_bytes()]).unwrap();

    let signed = LegalTask::sign(payload(3), &key).unwrap();
    let tampered = LegalTask::from_parts(
        payload(4),
        signed.authorizer_public_key(),
        signed.signature_bytes(),
    );

    assert_eq!(
        tampered.verify(&authorizers),
        Err(AuthorizationError::InvalidSignature)
    );
}

#[test]
fn untrusted_authorizer_is_rejected_even_with_a_valid_signature() {
    let trusted = signing_key(7);
    let untrusted = signing_key(9);
    let authorizers = AuthorizerSet::new(
        CURRENT_PROTOCOL_VERSION,
        [trusted.verifying_key().to_bytes()],
    )
    .unwrap();

    let task = LegalTask::sign(payload(3), &untrusted).unwrap();

    assert_eq!(
        task.verify(&authorizers),
        Err(AuthorizationError::UntrustedAuthorizer)
    );
}

#[test]
fn protocol_version_is_part_of_authorization_policy_and_signature_payload() {
    let key = signing_key(7);
    let authorizers =
        AuthorizerSet::new(CURRENT_PROTOCOL_VERSION, [key.verifying_key().to_bytes()]).unwrap();

    let payload = LegalTaskPayload::new(
        support::task_id(42),
        CURRENT_PROTOCOL_VERSION + 1,
        None,
        vec![Operation::Issue {
            account: support::account(1),
            count: 1,
        }],
    );
    let task = LegalTask::sign(payload, &key).unwrap();

    assert_eq!(
        task.verify(&authorizers),
        Err(AuthorizationError::UnsupportedProtocolVersion {
            expected: CURRENT_PROTOCOL_VERSION,
            actual: CURRENT_PROTOCOL_VERSION + 1,
        })
    );
}

#[test]
fn structurally_invalid_operations_never_become_verified_tasks() {
    let key = signing_key(7);
    let authorizers =
        AuthorizerSet::new(CURRENT_PROTOCOL_VERSION, [key.verifying_key().to_bytes()]).unwrap();

    let cases = [
        (
            Operation::Transfer {
                source: support::payment(1),
                destination: support::payment(2),
                amount: 0,
            },
            second::TaskValidationError::TransferAmountZero,
        ),
        (
            Operation::Issue {
                account: support::account(1),
                count: 0,
            },
            second::TaskValidationError::IssueAmountZero,
        ),
        (
            Operation::LeakRepair { leaked: Vec::new() },
            second::TaskValidationError::EmptyLeakRepair,
        ),
        (
            Operation::LeakRepair {
                leaked: vec![
                    second::CurrencyAddress::new(1),
                    second::CurrencyAddress::new(1),
                ],
            },
            second::TaskValidationError::DuplicateCurrency(second::CurrencyAddress::new(1)),
        ),
    ];

    for (index, (operation, expected)) in cases.into_iter().enumerate() {
        let task = LegalTask::sign(
            LegalTaskPayload::new(
                support::task_id(100 + index as u128),
                CURRENT_PROTOCOL_VERSION,
                Some(100),
                vec![operation],
            ),
            &key,
        )
        .unwrap();

        assert_eq!(
            task.verify(&authorizers),
            Err(AuthorizationError::InvalidPayload(expected))
        );
    }
}

#[test]
fn operation_order_changes_the_canonical_signed_message() {
    let key = signing_key(7);

    let first = LegalTask::sign(
        LegalTaskPayload::new(
            support::task_id(1),
            CURRENT_PROTOCOL_VERSION,
            None,
            vec![
                Operation::Issue {
                    account: support::account(1),
                    count: 1,
                },
                Operation::Transfer {
                    source: support::payment(1),
                    destination: support::payment(2),
                    amount: 1,
                },
            ],
        ),
        &key,
    )
    .unwrap();

    let second = LegalTask::sign(
        LegalTaskPayload::new(
            support::task_id(1),
            CURRENT_PROTOCOL_VERSION,
            None,
            vec![
                Operation::Transfer {
                    source: support::payment(1),
                    destination: support::payment(2),
                    amount: 1,
                },
                Operation::Issue {
                    account: support::account(1),
                    count: 1,
                },
            ],
        ),
        &key,
    )
    .unwrap();

    assert_ne!(
        first.canonical_signing_bytes().unwrap(),
        second.canonical_signing_bytes().unwrap()
    );
}
