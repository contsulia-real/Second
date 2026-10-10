use crate::legal_task_codec::{MAX_ENCODED_LEGAL_TASK_SIZE, decode_legal_task, encode_legal_task};
use crate::prepared_plan::{PreparedOperation, PreparedTask};
use crate::{
    AddressRange, AddressRanges, AuthorizationError, CurrencyAddress, LegalTask, Operation,
    PreparationError,
};

const LENGTH_PREFIX_SIZE: usize = size_of::<u32>();
const ENCODED_RANGE_SIZE: usize = 2 * size_of::<u64>();

// Signed requests and their irreducible frozen choices share one bounded budget.
pub(crate) const MAX_PREPARED_SOURCE_SIZE: usize = MAX_ENCODED_LEGAL_TASK_SIZE + LENGTH_PREFIX_SIZE;

pub(crate) fn encoded_source_length(
    request_length: usize,
    range_counts: impl IntoIterator<Item = usize>,
) -> Option<usize> {
    range_counts.into_iter().try_fold(
        request_length.checked_add(LENGTH_PREFIX_SIZE)?,
        |length, count| {
            length
                .checked_add(LENGTH_PREFIX_SIZE)?
                .checked_add(count.checked_mul(ENCODED_RANGE_SIZE)?)
        },
    )
}

pub(crate) fn validate_allocation_source(task: &LegalTask) -> Result<(), AuthorizationError> {
    let operations = task.payload().operations();
    if !operations.iter().any(|operation| {
        matches!(
            operation,
            Operation::Issue { .. } | Operation::LeakRepair { .. }
        )
    }) {
        return Ok(());
    }
    let request_length = encode_legal_task(task)
        .map_err(AuthorizationError::Encoding)?
        .len();
    let required = encoded_source_length(
        request_length,
        operations.iter().filter_map(|operation| match operation {
            Operation::LeakRepair { leaked } => Some(leaked.len()),
            // Fragmentation depends on business state; admission only budgets one run.
            Operation::Transfer { .. } => Some(1),
            _ => None,
        }),
    )
    .ok_or(AuthorizationError::Encoding(
        crate::TaskEncodingError::LengthOverflow,
    ))?;
    if required > MAX_PREPARED_SOURCE_SIZE {
        return Err(AuthorizationError::AllocationSourceTooLarge {
            maximum: MAX_PREPARED_SOURCE_SIZE,
            required,
        });
    }
    Ok(())
}

pub(crate) struct PreparedTaskSource {
    pub(crate) task: LegalTask,
    pub(crate) selections: Vec<AddressRanges>,
}

impl PreparedTaskSource {
    pub(crate) fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > MAX_PREPARED_SOURCE_SIZE {
            return None;
        }
        let (length, remainder) = bytes.split_at_checked(LENGTH_PREFIX_SIZE)?;
        let length = usize::try_from(u32::from_be_bytes(length.try_into().ok()?)).ok()?;
        if length > MAX_ENCODED_LEGAL_TASK_SIZE {
            return None;
        }
        let (legal, mut remaining) = remainder.split_at_checked(length)?;
        let task = decode_legal_task(legal)?;
        let mut selections = Vec::new();
        for operation in task.payload().operations() {
            let required = match operation {
                Operation::Transfer { amount, .. } => *amount,
                Operation::LeakRepair { leaked } => leaked.len() as u64,
                _ => continue,
            };
            let (count_bytes, rest) = remaining.split_at_checked(LENGTH_PREFIX_SIZE)?;
            let count = u32::from_be_bytes(count_bytes.try_into().ok()?) as usize;
            let (encoded, next) = rest.split_at_checked(count.checked_mul(ENCODED_RANGE_SIZE)?)?;
            let mut ranges = Vec::with_capacity(count);
            for bytes in encoded.as_chunks::<ENCODED_RANGE_SIZE>().0 {
                ranges.push(AddressRange::new(
                    CurrencyAddress::new(u64::from_be_bytes(bytes[..8].try_into().ok()?)),
                    u64::from_be_bytes(bytes[8..].try_into().ok()?),
                )?);
            }
            let selected = AddressRanges::from_canonical(ranges)?;
            if selected.len() != required {
                return None;
            }
            selections.push(selected);
            remaining = next;
        }
        remaining.is_empty().then_some(Self { task, selections })
    }
}

impl PreparedTask {
    pub(crate) fn encode_source(&self) -> Result<Vec<u8>, PreparationError> {
        let legal =
            encode_legal_task(&self.source_task).map_err(|_| PreparationError::LengthOverflow)?;
        let length = encoded_source_length(
            legal.len(),
            self.operations
                .iter()
                .filter_map(|operation| match operation {
                    PreparedOperation::Transfer { currencies, .. } => {
                        Some(currencies.ranges().len())
                    }
                    PreparedOperation::LeakRepair { reserve, .. } => Some(reserve.ranges().len()),
                    _ => None,
                }),
        )
        .ok_or(PreparationError::LengthOverflow)?;
        if legal.len() > MAX_ENCODED_LEGAL_TASK_SIZE || length > MAX_PREPARED_SOURCE_SIZE {
            return Err(PreparationError::LengthOverflow);
        }
        let mut out = Vec::with_capacity(length);
        out.extend_from_slice(
            &u32::try_from(legal.len())
                .map_err(|_| PreparationError::LengthOverflow)?
                .to_be_bytes(),
        );
        out.extend_from_slice(&legal);
        for operation in &self.operations {
            let selection = match operation {
                PreparedOperation::Transfer { currencies, .. } => currencies,
                PreparedOperation::LeakRepair { reserve, .. } => reserve,
                _ => continue,
            };
            out.extend_from_slice(&(selection.ranges().len() as u32).to_be_bytes());
            for range in selection.ranges() {
                out.extend_from_slice(&range.start.value().to_be_bytes());
                out.extend_from_slice(&range.len.to_be_bytes());
            }
        }
        Ok(out)
    }
}
