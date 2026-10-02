use crate::{
    AccountAddress, CurrencyAddress, LegalTask, LegalTaskPayload, Operation, PaymentAddress,
    TaskEncodingError, TaskId,
};

pub(crate) fn encode_legal_task(task: &LegalTask) -> Result<Vec<u8>, TaskEncodingError> {
    let payload = task.payload();
    let mut out = Vec::with_capacity(256);

    push_task_id(&mut out, &payload.task_id())?;
    out.extend_from_slice(&payload.protocol_version().to_be_bytes());
    match payload.expires_at() {
        Some(expires_at) => {
            out.push(1);
            out.extend_from_slice(&expires_at.to_be_bytes());
        }
        None => out.push(0),
    }

    push_len(&mut out, payload.operations().len())?;
    for operation in payload.operations() {
        encode_operation(&mut out, operation)?;
    }

    out.extend_from_slice(&task.authorizer_public_key());
    out.extend_from_slice(&task.signature_bytes());
    Ok(out)
}

pub(crate) fn decode_legal_task(bytes: &[u8]) -> Option<LegalTask> {
    let mut cursor = Cursor::new(bytes);
    let task_id = cursor.task_id()?;
    let protocol_version = cursor.u32()?;
    let expires_at = match cursor.u8()? {
        0 => None,
        1 => Some(cursor.u64()?),
        _ => return None,
    };

    let operation_count = cursor.len()?;
    if operation_count > cursor.remaining() {
        return None;
    }
    let mut operations = Vec::new();
    operations.try_reserve_exact(operation_count).ok()?;
    for _ in 0..operation_count {
        operations.push(decode_operation(&mut cursor)?);
    }

    let authorizer_public_key = cursor.array_32()?;
    let signature = cursor.array_64()?;
    if !cursor.finished() {
        return None;
    }

    let payload = LegalTaskPayload::new(task_id, protocol_version, expires_at, operations);
    payload.validate().ok()?;
    Some(LegalTask::from_parts(
        payload,
        authorizer_public_key,
        signature,
    ))
}

fn encode_operation(out: &mut Vec<u8>, operation: &Operation) -> Result<(), TaskEncodingError> {
    match operation {
        Operation::Transfer {
            source,
            destination,
            amount,
        } => {
            out.push(1);
            out.extend_from_slice(&source.bytes());
            out.extend_from_slice(&destination.bytes());
            out.extend_from_slice(&amount.to_be_bytes());
        }
        Operation::Issue { account, count } => {
            out.push(2);
            out.extend_from_slice(&account.bytes());
            out.extend_from_slice(&count.to_be_bytes());
        }
        Operation::Destroy { currencies } => {
            out.push(3);
            push_currency_addresses(out, currencies)?;
        }
        Operation::LeakRepair { leaked } => {
            out.push(4);
            push_currency_addresses(out, leaked)?;
        }
        Operation::RegisterPaymentAddress { address, account } => {
            out.push(5);
            out.extend_from_slice(&address.bytes());
            out.extend_from_slice(&account.bytes());
        }
        Operation::RetirePaymentAddress { address } => {
            out.push(6);
            out.extend_from_slice(&address.bytes());
        }
        Operation::FinalizePaymentAddressRetirement { address } => {
            out.push(7);
            out.extend_from_slice(&address.bytes());
        }
    }
    Ok(())
}

fn decode_operation(cursor: &mut Cursor<'_>) -> Option<Operation> {
    match cursor.u8()? {
        1 => Some(Operation::Transfer {
            source: PaymentAddress::from_bytes(cursor.array_32()?),
            destination: PaymentAddress::from_bytes(cursor.array_32()?),
            amount: cursor.u64()?,
        }),
        2 => Some(Operation::Issue {
            account: AccountAddress::from_bytes(cursor.array_32()?),
            count: cursor.u64()?,
        }),
        3 => Some(Operation::Destroy {
            currencies: decode_currency_addresses(cursor)?,
        }),
        4 => Some(Operation::LeakRepair {
            leaked: decode_currency_addresses(cursor)?,
        }),
        5 => Some(Operation::RegisterPaymentAddress {
            address: PaymentAddress::from_bytes(cursor.array_32()?),
            account: AccountAddress::from_bytes(cursor.array_32()?),
        }),
        6 => Some(Operation::RetirePaymentAddress {
            address: PaymentAddress::from_bytes(cursor.array_32()?),
        }),
        7 => Some(Operation::FinalizePaymentAddressRetirement {
            address: PaymentAddress::from_bytes(cursor.array_32()?),
        }),
        _ => None,
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

fn decode_currency_addresses(cursor: &mut Cursor<'_>) -> Option<Vec<CurrencyAddress>> {
    let count = cursor.len()?;
    if count > cursor.remaining() / size_of::<u64>() {
        return None;
    }
    let mut addresses = Vec::new();
    addresses.try_reserve_exact(count).ok()?;
    for _ in 0..count {
        addresses.push(CurrencyAddress::new(cursor.u64()?));
    }
    Some(addresses)
}

fn push_task_id(out: &mut Vec<u8>, task_id: &TaskId) -> Result<(), TaskEncodingError> {
    let len = u8::try_from(task_id.len()).map_err(|_| TaskEncodingError::LengthOverflow)?;
    out.push(len);
    out.extend_from_slice(task_id.as_bytes());
    Ok(())
}

fn push_len(out: &mut Vec<u8>, len: usize) -> Result<(), TaskEncodingError> {
    let len = u32::try_from(len).map_err(|_| TaskEncodingError::LengthOverflow)?;
    out.extend_from_slice(&len.to_be_bytes());
    Ok(())
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    fn finished(&self) -> bool {
        self.offset == self.bytes.len()
    }

    fn exact(&mut self, len: usize) -> Option<&'a [u8]> {
        let end = self.offset.checked_add(len)?;
        let value = self.bytes.get(self.offset..end)?;
        self.offset = end;
        Some(value)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(*self.exact(1)?.first()?)
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.exact(4)?.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_be_bytes(self.exact(8)?.try_into().ok()?))
    }

    fn len(&mut self) -> Option<usize> {
        usize::try_from(self.u32()?).ok()
    }

    fn array_32(&mut self) -> Option<[u8; 32]> {
        self.exact(32)?.try_into().ok()
    }

    fn array_64(&mut self) -> Option<[u8; 64]> {
        self.exact(64)?.try_into().ok()
    }

    fn task_id(&mut self) -> Option<TaskId> {
        let len = usize::from(self.u8()?);
        TaskId::from_ascii_bytes(self.exact(len)?).ok()
    }
}
