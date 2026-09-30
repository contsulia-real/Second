use serde::Deserialize;

use crate::{
    AccountAddress, CURRENT_PROTOCOL_VERSION, CurrencyAddress, LegalTask, LegalTaskPayload,
    Operation, PaymentAddress,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransactionRequestParseError {
    InvalidJson,
    UnsupportedProtocolVersion,
    InvalidRequestId,
    InvalidAccountAddress,
    InvalidPaymentAddress,
    InvalidCurrencyAddress,
    InvalidSignature,
    InvalidOperation,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTransactionRequest {
    request_id: String,
    version: u32,
    expires_at: Option<u64>,
    operations: Vec<RawOperation>,
    signature: String,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum RawOperation {
    Transfer {
        source: String,
        destination: String,
        amount: u64,
    },
    Issue {
        recipient: String,
        amount: u64,
    },
    Destroy {
        currencies: Vec<String>,
    },
    LeakRepair {
        currencies: Vec<String>,
    },
}

pub fn parse_transaction_request_json(
    input: &[u8],
    issuer_public_key: [u8; 32],
) -> Result<LegalTask, TransactionRequestParseError> {
    let request: RawTransactionRequest =
        serde_json::from_slice(input).map_err(|_| TransactionRequestParseError::InvalidJson)?;

    if request.version != CURRENT_PROTOCOL_VERSION {
        return Err(TransactionRequestParseError::UnsupportedProtocolVersion);
    }

    let task_id = crate::TaskId::parse(&request.request_id)
        .map_err(|_| TransactionRequestParseError::InvalidRequestId)?;
    let operations = request
        .operations
        .into_iter()
        .map(parse_operation)
        .collect::<Result<Vec<_>, _>>()?;

    let payload = LegalTaskPayload::new(task_id, request.version, request.expires_at, operations);
    payload
        .validate()
        .map_err(|_| TransactionRequestParseError::InvalidOperation)?;

    LegalTask::from_signature_base64url(payload, issuer_public_key, &request.signature)
        .map_err(|_| TransactionRequestParseError::InvalidSignature)
}

fn parse_operation(operation: RawOperation) -> Result<Operation, TransactionRequestParseError> {
    match operation {
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
        RawOperation::Destroy { currencies } => Ok(Operation::Destroy {
            currencies: parse_currency_addresses(currencies)?,
        }),
        RawOperation::LeakRepair { currencies } => Ok(Operation::LeakRepair {
            leaked: parse_currency_addresses(currencies)?,
        }),
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
