use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};

use crate::{
    AccountAddress, CURRENT_PROTOCOL_VERSION, CurrencyAddress, LegalTask, LegalTaskPayload,
    Operation, PaymentAddress,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransactionRequestParseError {
    InvalidJson,
    InvalidNetworkId,
    UnsupportedProtocolVersion,
    InvalidRequestId,
    InvalidAccountAddress,
    InvalidPaymentAddress,
    InvalidCurrencyAddress,
    InvalidSignature,
    InvalidOperation,
    Encoding,
    TooLarge,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawTransactionRequest {
    request_id: String,
    version: u32,
    expires_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    network_id_base64: Option<String>,
    operations: Vec<RawOperation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    signature: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    account_signatures: Vec<RawAccountSignature>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawAccountSignature {
    account: String,
    signature: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawUnsignedTransaction {
    request_id: String,
    version: u32,
    expires_at: Option<u64>,
    #[serde(default)]
    network_id_base64: Option<String>,
    operations: Vec<RawOperation>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum RawOperation {
    RegisterAccount {
        account: String,
    },
    Transfer {
        source: String,
        destination: String,
        amount: u64,
    },
    Issue {
        recipient: String,
        amount: u64,
    },
    LeakRepair {
        currencies: Vec<String>,
    },
    RegisterPaymentAddress {
        address: String,
        account: String,
    },
    RetirePaymentAddress {
        address: String,
    },
    FinalizePaymentAddressRetirement {
        address: String,
    },
}

pub fn parse_transaction_request_json(
    input: &[u8],
    issuer_public_key: [u8; 32],
) -> Result<LegalTask, TransactionRequestParseError> {
    let request: RawTransactionRequest =
        serde_json::from_slice(input).map_err(|_| TransactionRequestParseError::InvalidJson)?;

    let payload = parse_payload(
        request.request_id,
        request.version,
        request.expires_at,
        request.operations,
    )?;
    let network_id = decode_network_id(request.network_id_base64.as_deref())?;
    let mut task = match request.signature {
        Some(signature) => LegalTask::from_signature_base64url_in_network(
            payload,
            network_id,
            issuer_public_key,
            &signature,
        )
        .map_err(|_| TransactionRequestParseError::InvalidSignature)?,
        None if issuer_public_key == [0; 32] => {
            LegalTask::from_signed_parts(payload, network_id, [0; 32], [0; 64], Vec::new())
        }
        None => return Err(TransactionRequestParseError::InvalidSignature),
    };
    for entry in request.account_signatures {
        let account = AccountAddress::parse(&entry.account)
            .map_err(|_| TransactionRequestParseError::InvalidAccountAddress)?;
        if task.has_account_signature(account) {
            return Err(TransactionRequestParseError::InvalidSignature);
        }
        task.with_account_signature_base64url(account, &entry.signature)
            .map_err(|_| TransactionRequestParseError::InvalidSignature)?;
    }
    task.verify_account_signatures()
        .map_err(|_| TransactionRequestParseError::InvalidSignature)?;
    Ok(task)
}

/// Sign strict unsigned transaction JSON using the same validation and canonical
/// payload encoding as submission. Input must not contain a signature field.
pub fn sign_transaction_request_json(
    input: &[u8],
    signing_key: &SigningKey,
) -> Result<Vec<u8>, TransactionRequestParseError> {
    let request: RawUnsignedTransaction =
        serde_json::from_slice(input).map_err(|_| TransactionRequestParseError::InvalidJson)?;
    let payload = parse_payload(
        request.request_id.clone(),
        request.version,
        request.expires_at,
        request.operations.clone(),
    )?;
    let task = LegalTask::sign_in_network(
        payload,
        signing_key,
        decode_network_id(request.network_id_base64.as_deref())?,
    )
    .map_err(|_| TransactionRequestParseError::Encoding)?;
    let encoded = crate::legal_task_codec::encode_legal_task(&task)
        .map_err(|_| TransactionRequestParseError::Encoding)?;
    if encoded.len() > crate::legal_task_codec::MAX_ENCODED_LEGAL_TASK_SIZE {
        return Err(TransactionRequestParseError::TooLarge);
    }
    serde_json::to_vec_pretty(&RawTransactionRequest {
        request_id: request.request_id,
        version: request.version,
        expires_at: request.expires_at,
        network_id_base64: request.network_id_base64,
        operations: request.operations,
        signature: Some(task.signature_base64url()),
        account_signatures: Vec::new(),
    })
    .map_err(|_| TransactionRequestParseError::Encoding)
}

/// The account owner signs the exact canonical payload; an Authorizer is not required.
pub fn sign_account_transaction_request_json(
    input: &[u8],
    key: &SigningKey,
) -> Result<Vec<u8>, TransactionRequestParseError> {
    let request: RawUnsignedTransaction =
        serde_json::from_slice(input).map_err(|_| TransactionRequestParseError::InvalidJson)?;
    let payload = parse_payload(
        request.request_id.clone(),
        request.version,
        request.expires_at,
        request.operations.clone(),
    )?;
    let task = LegalTask::sign_account_in_network(
        payload,
        key,
        decode_network_id(request.network_id_base64.as_deref())?,
    )
    .map_err(|_| TransactionRequestParseError::Encoding)?;
    let encoded = crate::legal_task_codec::encode_legal_task(&task)
        .map_err(|_| TransactionRequestParseError::Encoding)?;
    if encoded.len() > crate::legal_task_codec::MAX_ENCODED_LEGAL_TASK_SIZE {
        return Err(TransactionRequestParseError::TooLarge);
    }
    let account = AccountAddress::from_bytes(key.verifying_key().to_bytes());
    serde_json::to_vec_pretty(&RawTransactionRequest {
        request_id: request.request_id,
        version: request.version,
        expires_at: request.expires_at,
        network_id_base64: request.network_id_base64,
        operations: request.operations,
        signature: None,
        account_signatures: vec![RawAccountSignature {
            account: account.to_string(),
            signature: base64::Engine::encode(
                &base64::engine::general_purpose::URL_SAFE_NO_PAD,
                task.account_signatures()[0].signature,
            ),
        }],
    })
    .map_err(|_| TransactionRequestParseError::Encoding)
}

fn decode_network_id(encoded: Option<&str>) -> Result<[u8; 32], TransactionRequestParseError> {
    use base64::Engine as _;
    let Some(encoded) = encoded else {
        return Ok([0; 32]);
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| TransactionRequestParseError::InvalidNetworkId)?;
    let network_id: [u8; 32] = bytes
        .try_into()
        .map_err(|_| TransactionRequestParseError::InvalidNetworkId)?;
    if base64::engine::general_purpose::STANDARD.encode(network_id) != encoded {
        return Err(TransactionRequestParseError::InvalidNetworkId);
    }
    Ok(network_id)
}

fn parse_payload(
    request_id: String,
    version: u32,
    expires_at: Option<u64>,
    raw_operations: Vec<RawOperation>,
) -> Result<LegalTaskPayload, TransactionRequestParseError> {
    if version != CURRENT_PROTOCOL_VERSION {
        return Err(TransactionRequestParseError::UnsupportedProtocolVersion);
    }

    let task_id = crate::TaskId::parse(&request_id)
        .map_err(|_| TransactionRequestParseError::InvalidRequestId)?;
    let operations = raw_operations
        .into_iter()
        .map(parse_operation)
        .collect::<Result<Vec<_>, _>>()?;

    let payload = LegalTaskPayload::new(task_id, version, expires_at, operations);
    payload
        .validate()
        .map_err(|_| TransactionRequestParseError::InvalidOperation)?;

    Ok(payload)
}

fn parse_operation(operation: RawOperation) -> Result<Operation, TransactionRequestParseError> {
    match operation {
        RawOperation::RegisterAccount { account } => Ok(Operation::RegisterAccount {
            account: AccountAddress::parse(&account)
                .map_err(|_| TransactionRequestParseError::InvalidAccountAddress)?,
        }),
        RawOperation::Transfer {
            source,
            destination,
            amount,
        } => Ok(Operation::Transfer {
            source: PaymentAddress::parse(&source)
                .map_err(|_| TransactionRequestParseError::InvalidPaymentAddress)?,
            destination: PaymentAddress::parse(&destination)
                .map_err(|_| TransactionRequestParseError::InvalidPaymentAddress)?,
            amount,
        }),
        RawOperation::Issue { recipient, amount } => Ok(Operation::Issue {
            account: AccountAddress::parse(&recipient)
                .map_err(|_| TransactionRequestParseError::InvalidAccountAddress)?,
            count: amount,
        }),
        RawOperation::LeakRepair { currencies } => Ok(Operation::LeakRepair {
            leaked: parse_currency_addresses(currencies)?,
        }),
        RawOperation::RegisterPaymentAddress { address, account } => {
            Ok(Operation::RegisterPaymentAddress {
                address: PaymentAddress::parse(&address)
                    .map_err(|_| TransactionRequestParseError::InvalidPaymentAddress)?,
                account: AccountAddress::parse(&account)
                    .map_err(|_| TransactionRequestParseError::InvalidAccountAddress)?,
            })
        }
        RawOperation::RetirePaymentAddress { address } => Ok(Operation::RetirePaymentAddress {
            address: PaymentAddress::parse(&address)
                .map_err(|_| TransactionRequestParseError::InvalidPaymentAddress)?,
        }),
        RawOperation::FinalizePaymentAddressRetirement { address } => {
            Ok(Operation::FinalizePaymentAddressRetirement {
                address: PaymentAddress::parse(&address)
                    .map_err(|_| TransactionRequestParseError::InvalidPaymentAddress)?,
            })
        }
    }
}

fn parse_currency_addresses(
    currencies: Vec<String>,
) -> Result<Vec<CurrencyAddress>, TransactionRequestParseError> {
    currencies
        .into_iter()
        .map(|address| {
            CurrencyAddress::parse(&address)
                .map_err(|_| TransactionRequestParseError::InvalidCurrencyAddress)
        })
        .collect()
}
