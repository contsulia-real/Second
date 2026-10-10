//! Quorum-certified reservations of the shared Currency identity frontier.
use crate::{
    BftProposalSubject, CURRENT_PROTOCOL_VERSION, ConsensusScope, ExecutionError,
    FinalityStatement, LegalTask, Operation, TaskId, VerifiedLegalTask,
};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CurrencyAllocation {
    pub(crate) validator_set_version: u64,
    pub(crate) start: u64,
    pub(crate) count: u64,
    pub(crate) task: LegalTask,
}

pub(crate) fn required_count(task: &VerifiedLegalTask) -> Result<u64, ExecutionError> {
    required_count_for_operations(task.operations())
}

pub(crate) fn required_count_for_operations(
    operations: &[Operation],
) -> Result<u64, ExecutionError> {
    operations.iter().try_fold(0_u64, |total, operation| {
        let count = match operation {
            Operation::Issue { count, .. } => *count,
            Operation::LeakRepair { leaked } => u64::try_from(leaked.len())
                .map_err(|_| ExecutionError::CurrencySequenceSpaceExhausted)?,
            _ => 0,
        };
        total
            .checked_add(count)
            .ok_or(ExecutionError::CurrencySequenceSpaceExhausted)
    })
}

impl CurrencyAllocation {
    pub fn new(
        task: &VerifiedLegalTask,
        validator_set_version: u64,
        start: u64,
    ) -> Result<Self, ExecutionError> {
        let count = required_count(task)?;
        if count == 0 || start.checked_add(count).is_none() {
            return Err(ExecutionError::CurrencySequenceSpaceExhausted);
        }
        Ok(Self {
            validator_set_version,
            start,
            count,
            task: task.signed_task().clone(),
        })
    }

    pub fn task_id(&self) -> TaskId {
        self.task.payload().task_id()
    }
    pub fn start(&self) -> u64 {
        self.start
    }
    pub fn count(&self) -> u64 {
        self.count
    }
    pub fn scope(&self) -> ConsensusScope {
        ConsensusScope::CurrencyAllocation {
            validator_set_version: self.validator_set_version,
            start: self.start,
        }
    }
    pub fn digest(&self) -> [u8; 32] {
        Self::digest_for(
            self.validator_set_version,
            self.start,
            self.count,
            self.task
                .request_digest()
                .expect("validated allocation task"),
        )
    }
    pub(crate) fn digest_for(
        version: u64,
        start: u64,
        count: u64,
        request_digest: [u8; 32],
    ) -> [u8; 32] {
        let mut hash = Sha256::new();
        hash.update(b"SECOND_CURRENCY_ALLOCATION_V1\0");
        hash.update(version.to_be_bytes());
        hash.update(start.to_be_bytes());
        hash.update(count.to_be_bytes());
        // LegalTask was structurally validated when constructing this allocation.
        hash.update(request_digest);
        hash.finalize().into()
    }
    pub fn finality_statement(&self) -> FinalityStatement {
        FinalityStatement::new(
            CURRENT_PROTOCOL_VERSION,
            self.validator_set_version,
            self.digest(),
        )
    }
    pub(crate) fn subject(&self) -> BftProposalSubject {
        BftProposalSubject::new(self.validator_set_version, self.scope(), self.digest())
    }
}
