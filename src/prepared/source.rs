use crate::legal_task_codec::{MAX_ENCODED_LEGAL_TASK_SIZE, decode_legal_task, encode_legal_task};
use crate::prepared_plan::{PreparedOperation, PreparedTask};
use crate::{CurrencyAddress, LegalTask, Operation, PreparationError};

// Signed requests and their irreducible frozen choices share one bounded budget.
pub(crate) const MAX_PREPARED_SOURCE_SIZE: usize = MAX_ENCODED_LEGAL_TASK_SIZE + 4;

pub(crate) struct PreparedTaskSource {
    pub(crate) task: LegalTask,
    pub(crate) selections: Vec<Vec<CurrencyAddress>>,
}

impl PreparedTaskSource {
    pub(crate) fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > MAX_PREPARED_SOURCE_SIZE {
            return None;
        }
        let (length, remainder) = bytes.split_at_checked(4)?;
        let length = usize::try_from(u32::from_be_bytes(length.try_into().ok()?)).ok()?;
        if length > MAX_ENCODED_LEGAL_TASK_SIZE {
            return None;
        }
        let (legal, mut remaining) = remainder.split_at_checked(length)?;
        let task = decode_legal_task(legal)?;
        let mut selections = Vec::new();
        for operation in task.payload().operations() {
            let count = match operation {
                Operation::Transfer { amount, .. } => usize::try_from(*amount).ok()?,
                Operation::LeakRepair { leaked } => leaked.len(),
                _ => continue,
            };
            let size = count.checked_mul(8)?;
            let (addresses, next) = remaining.split_at_checked(size)?;
            let mut selected = Vec::new();
            selected.try_reserve_exact(count).ok()?;
            for address in addresses.as_chunks::<8>().0 {
                selected.push(CurrencyAddress::new(u64::from_be_bytes(*address)));
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
        let mut length = 4_usize
            .checked_add(legal.len())
            .ok_or(PreparationError::LengthOverflow)?;
        for operation in &self.operations {
            let selection = match operation {
                PreparedOperation::Transfer { currencies, .. } => currencies,
                PreparedOperation::LeakRepair { reserve, .. } => reserve,
                _ => continue,
            };
            length = length
                .checked_add(
                    selection
                        .len()
                        .checked_mul(8)
                        .ok_or(PreparationError::LengthOverflow)?,
                )
                .ok_or(PreparationError::LengthOverflow)?;
        }
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
            for address in selection {
                out.extend_from_slice(&address.value().to_be_bytes());
            }
        }
        Ok(out)
    }
}
