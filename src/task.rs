use crate::{AccountAddress, CurrencyAddress, TaskEncodingError, TaskId};

pub const CURRENT_PROTOCOL_VERSION: u32 = 1;

const SIGNING_DOMAIN: &[u8] = b"SECOND_LEGAL_TASK_V1\0";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Operation {
    Transfer {
        source: AccountAddress,
        destination: AccountAddress,
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
    expires_at: Option<u64>,
    operations: Vec<Operation>,
}

impl LegalTaskPayload {
    pub fn new(
        task_id: TaskId,
        protocol_version: u32,
        expires_at: Option<u64>,
        operations: Vec<Operation>,
    ) -> Self {
        Self {
            task_id,
            protocol_version,
            expires_at,
            operations,
        }
    }

    pub const fn task_id(&self) -> TaskId {
        self.task_id
    }

    pub const fn protocol_version(&self) -> u32 {
        self.protocol_version
    }

    pub const fn expires_at(&self) -> Option<u64> {
        self.expires_at
    }

    pub fn operations(&self) -> &[Operation] {
        &self.operations
    }

    pub fn canonical_signing_bytes(&self) -> Result<Vec<u8>, TaskEncodingError> {
        let mut out = Vec::with_capacity(128);
        out.extend_from_slice(SIGNING_DOMAIN);
        out.extend_from_slice(&self.protocol_version.to_be_bytes());
        out.extend_from_slice(&self.task_id.value().to_be_bytes());

        match self.expires_at {
            Some(expires_at) => {
                out.push(1);
                out.extend_from_slice(&expires_at.to_be_bytes());
            }
            None => out.push(0),
        }

        push_len(&mut out, self.operations.len())?;

        for operation in &self.operations {
            match operation {
                Operation::Transfer {
                    source,
                    destination,
                    amount,
                } => {
                    out.push(1);
                    out.extend_from_slice(&source.value().to_be_bytes());
                    out.extend_from_slice(&destination.value().to_be_bytes());
                    out.extend_from_slice(&amount.to_be_bytes());
                }
                Operation::Issue { account, count } => {
                    out.push(2);
                    out.extend_from_slice(&account.value().to_be_bytes());
                    out.extend_from_slice(&count.to_be_bytes());
                }
                Operation::Destroy { currencies } => {
                    out.push(3);
                    push_currency_addresses(&mut out, currencies)?;
                }
                Operation::LeakRepair { leaked } => {
                    out.push(4);
                    push_currency_addresses(&mut out, leaked)?;
                }
            }
        }

        Ok(out)
    }
}

fn push_currency_addresses(
    out: &mut Vec<u8>,
    addresses: &[CurrencyAddress],
) -> Result<(), TaskEncodingError> {
    push_len(out, addresses.len())?;
    for address in addresses {
        out.extend_from_slice(&address.value().to_be_bytes());
    }
    Ok(())
}

fn push_len(out: &mut Vec<u8>, len: usize) -> Result<(), TaskEncodingError> {
    let len = u64::try_from(len).map_err(|_| TaskEncodingError::LengthOverflow)?;
    out.extend_from_slice(&len.to_be_bytes());
    Ok(())
}
