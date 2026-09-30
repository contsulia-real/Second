use std::collections::BTreeSet;

use crate::{
    AccountAddress, CurrencyAddress, PaymentAddress, TaskEncodingError, TaskId, TaskValidationError,
};

pub const CURRENT_PROTOCOL_VERSION: u32 = 1;

const SIGNING_DOMAIN: &[u8] = b"Second/LegalTask/v1\0";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Operation {
    Transfer {
        source: PaymentAddress,
        destination: PaymentAddress,
        amount: u64,
    },
    Issue {
        account: AccountAddress,
        count: u64,
    },
    Destroy {
        currencies: Vec<CurrencyAddress>,
    },
    LeakRepair {
        leaked: Vec<CurrencyAddress>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegalTaskPayload {
    task_id: TaskId,
    protocol_version: u32,
    expires_at: u64,
    operations: Vec<Operation>,
}

impl LegalTaskPayload {
    pub fn new(
        task_id: TaskId,
        protocol_version: u32,
        expires_at: u64,
        operations: Vec<Operation>,
    ) -> Self {
        Self {
            task_id,
            protocol_version,
            expires_at,
            operations,
        }
    }

    pub fn task_id(&self) -> TaskId {
        self.task_id.clone()
    }

    pub const fn protocol_version(&self) -> u32 {
        self.protocol_version
    }

    pub const fn expires_at(&self) -> u64 {
        self.expires_at
    }

    pub fn operations(&self) -> &[Operation] {
        &self.operations
    }

    pub fn validate(&self) -> Result<(), TaskValidationError> {
        for operation in &self.operations {
            match operation {
                Operation::Transfer { amount, .. } if *amount == 0 => {
                    return Err(TaskValidationError::TransferAmountZero);
                }
                Operation::Issue { count, .. } if *count == 0 => {
                    return Err(TaskValidationError::IssueAmountZero);
                }
                Operation::Destroy { currencies } => {
                    validate_currency_set(currencies, TaskValidationError::EmptyDestroy)?;
                }
                Operation::LeakRepair { leaked } => {
                    validate_currency_set(leaked, TaskValidationError::EmptyLeakRepair)?;
                }
                Operation::Transfer { .. } | Operation::Issue { .. } => {}
            }
        }

        Ok(())
    }

    pub fn canonical_signing_bytes(&self) -> Result<Vec<u8>, TaskEncodingError> {
        let mut out = Vec::with_capacity(256);
        out.extend_from_slice(SIGNING_DOMAIN);

        push_array_len(&mut out, 2)?;
        push_text(&mut out, self.task_id.as_str())?;

        push_array_len(&mut out, 3)?;
        push_unsigned(&mut out, u64::from(self.protocol_version));
        push_unsigned(&mut out, self.expires_at);
        push_array_len(&mut out, self.operations.len())?;

        for operation in &self.operations {
            encode_operation(&mut out, operation)?;
        }

        Ok(out)
    }
}

fn encode_operation(out: &mut Vec<u8>, operation: &Operation) -> Result<(), TaskEncodingError> {
    match operation {
        Operation::Transfer {
            source,
            destination,
            amount,
        } => {
            push_array_len(out, 4)?;
            push_unsigned(out, 0);
            push_text(out, &source.canonical_string())?;
            push_text(out, &destination.canonical_string())?;
            push_unsigned(out, *amount);
        }
        Operation::Issue { account, count } => {
            push_array_len(out, 3)?;
            push_unsigned(out, 1);
            push_text(out, &account.canonical_string())?;
            push_unsigned(out, *count);
        }
        Operation::Destroy { currencies } => {
            push_array_len(out, 2)?;
            push_unsigned(out, 2);
            push_currency_addresses(out, currencies)?;
        }
        Operation::LeakRepair { leaked } => {
            push_array_len(out, 2)?;
            push_unsigned(out, 3);
            push_currency_addresses(out, leaked)?;
        }
    }

    Ok(())
}

fn validate_currency_set(
    addresses: &[CurrencyAddress],
    empty_error: TaskValidationError,
) -> Result<(), TaskValidationError> {
    if addresses.is_empty() {
        return Err(empty_error);
    }

    let mut seen = BTreeSet::new();
    for address in addresses {
        if !seen.insert(*address) {
            return Err(TaskValidationError::DuplicateCurrency(*address));
        }
    }

    Ok(())
}

fn push_currency_addresses(
    out: &mut Vec<u8>,
    addresses: &[CurrencyAddress],
) -> Result<(), TaskEncodingError> {
    push_array_len(out, addresses.len())?;
    for address in addresses {
        push_text(out, &address.canonical_string())?;
    }
    Ok(())
}

fn push_array_len(out: &mut Vec<u8>, len: usize) -> Result<(), TaskEncodingError> {
    let len = u64::try_from(len).map_err(|_| TaskEncodingError::LengthOverflow)?;
    push_major_value(out, 4, len);
    Ok(())
}

fn push_text(out: &mut Vec<u8>, value: &str) -> Result<(), TaskEncodingError> {
    let len = u64::try_from(value.len()).map_err(|_| TaskEncodingError::LengthOverflow)?;
    push_major_value(out, 3, len);
    out.extend_from_slice(value.as_bytes());
    Ok(())
}

fn push_unsigned(out: &mut Vec<u8>, value: u64) {
    push_major_value(out, 0, value);
}

fn push_major_value(out: &mut Vec<u8>, major: u8, value: u64) {
    let prefix = major << 5;
    match value {
        0..=23 => out.push(prefix | value as u8),
        24..=0xff => {
            out.push(prefix | 24);
            out.push(value as u8);
        }
        0x100..=0xffff => {
            out.push(prefix | 25);
            out.extend_from_slice(&(value as u16).to_be_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            out.push(prefix | 26);
            out.extend_from_slice(&(value as u32).to_be_bytes());
        }
        _ => {
            out.push(prefix | 27);
            out.extend_from_slice(&value.to_be_bytes());
        }
    }
}
