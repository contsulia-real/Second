use ed25519_dalek::SigningKey;
use second::{
    AccountAddress, AuthorizationError, AuthorizerSet, CURRENT_PROTOCOL_VERSION, LegalTask,
    LegalTaskPayload, Operation, TaskId,
};

fn signing_key(byte: u8) -> SigningKey {
    SigningKey::from_bytes(&[byte; 32])
}

fn payload(amount: u64) -> LegalTaskPayload {
    LegalTaskPayload::new(
        TaskId::new(42),
        CURRENT_PROTOCOL_VERSION,
        Some(100),
        vec![Operation::Transfer {
            source: AccountAddress::new(1),
            destination: AccountAddress::new(2),
            amount,
        }],
    )
}

#[test]
fn valid_authorizer_signature_verifies_and_produces_stable_request_digest() {
    let key = signing_key(7);
    let authorizers =
        AuthorizerSet::new(CURRENT_PROTOCOL_VERSION, [key.verifying_key().to_bytes()]).unwrap();

    let first = LegalTask::sign(payload(3), &key).unwrap();
    let second = LegalTask::sign(payload(3), &key).unwrap();

    let first_verified = first.verify(&authorizers).unwrap();
    let second_verified = second.verify(&authorizers).unwrap();

    assert_eq!(
        first_verified.request_digest(),
        second_verified.request_digest()
    );
    assert_eq!(
        first.canonical_signing_bytes().unwrap(),
        second.canonical_signing_bytes().unwrap()
    );
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
        TaskId::new(42),
        CURRENT_PROTOCOL_VERSION + 1,
        None,
        vec![Operation::Issue {
            account: AccountAddress::new(1),
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
fn operation_order_changes_the_canonical_signed_message() {
    let key = signing_key(7);

    let first = LegalTask::sign(
        LegalTaskPayload::new(
            TaskId::new(1),
            CURRENT_PROTOCOL_VERSION,
            None,
            vec![
                Operation::Issue {
                    account: AccountAddress::new(1),
                    count: 1,
                },
                Operation::Transfer {
                    source: AccountAddress::new(1),
                    destination: AccountAddress::new(2),
                    amount: 1,
                },
            ],
        ),
        &key,
    )
    .unwrap();

    let second = LegalTask::sign(
        LegalTaskPayload::new(
            TaskId::new(1),
            CURRENT_PROTOCOL_VERSION,
            None,
            vec![
                Operation::Transfer {
                    source: AccountAddress::new(1),
                    destination: AccountAddress::new(2),
                    amount: 1,
                },
                Operation::Issue {
                    account: AccountAddress::new(1),
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
