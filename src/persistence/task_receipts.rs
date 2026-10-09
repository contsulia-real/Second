//! Bounded local replay data, atomically retained when certified work commits.
use super::codec::{Decoder, push_len};
use super::local_codec::{decode_prepared_tasks, encode_prepared_task};
use crate::prepared_plan::{PreparedTask, PreparedTaskPhase};
use crate::{
    FinalityCertificate, PersistedNodeState, PersistenceError, SecondState, TaskId, ValidatorSet,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "task_receipts_tests.rs"]
mod tests;

pub(crate) const MAX_TASK_RECEIPTS: usize = 64;
const MAX_TASK_RECEIPT_BYTES: usize = 128 * 1024 * 1024;
pub(crate) type TaskReceipts = BTreeMap<TaskId, Arc<TaskReceipt>>;

pub(crate) struct TaskReceipt {
    generation: u64,
    plan: PreparedTask,
    allocation: Option<(u64, FinalityCertificate)>,
    encoded: Vec<u8>,
    verified_for: OnceLock<(ValidatorSet, Option<ValidatorSet>)>,
    source: OnceLock<Arc<Vec<u8>>>,
}

impl TaskReceipt {
    pub(crate) fn source(&self) -> Result<Arc<Vec<u8>>, PersistenceError> {
        if let Some(source) = self.source.get() {
            return Ok(Arc::clone(source));
        }
        let source = Arc::new(
            self.plan
                .encode_source()
                .map_err(|_| PersistenceError::InvalidSnapshot)?,
        );
        let _ = self.source.set(Arc::clone(&source));
        Ok(source)
    }
    pub(crate) fn plan(&self) -> &PreparedTask {
        &self.plan
    }
    pub(crate) fn allocation(&self) -> Option<&(u64, FinalityCertificate)> {
        self.allocation.as_ref()
    }
    fn new(
        generation: u64,
        mut plan: PreparedTask,
        allocation: Option<(u64, FinalityCertificate)>,
    ) -> Result<Self, PersistenceError> {
        plan.variants.clear();
        plan.commit_authorized = false;
        plan.conflict_abort = false;
        let mut encoded = generation.to_be_bytes().to_vec();
        push_len(&mut encoded, 1)?;
        encode_prepared_task(&mut encoded, &plan)?;
        match &allocation {
            Some((start, certificate)) => {
                encoded.push(1);
                encoded.extend_from_slice(&start.to_be_bytes());
                let bytes = crate::finality_codec::encode_certificate(certificate)
                    .ok_or(PersistenceError::InvalidSnapshot)?;
                push_len(&mut encoded, bytes.len())?;
                encoded.extend_from_slice(&bytes);
            }
            None => encoded.push(0),
        }
        Ok(Self {
            generation,
            plan,
            allocation,
            encoded,
            verified_for: OnceLock::new(),
            source: OnceLock::new(),
        })
    }

    pub(crate) fn certificate(&self) -> Result<FinalityCertificate, PersistenceError> {
        self.plan
            .finality_certificate()
            .map_err(|_| PersistenceError::InvalidSnapshot)?
            .ok_or(PersistenceError::InvalidSnapshot)
    }
}

pub(super) fn after_commit(
    latest: &PersistedNodeState,
    state: &SecondState,
    remaining: &BTreeMap<TaskId, PreparedTask>,
) -> Result<TaskReceipts, PersistenceError> {
    let mut receipts = latest.task_receipts.clone();
    for (task_id, plan) in &latest.prepared_tasks {
        if remaining.contains_key(task_id) || state.task_succeeded(task_id.clone()) != Some(true) {
            continue;
        }
        if plan.phase != PreparedTaskPhase::Finalized {
            return Err(PersistenceError::InvalidSnapshot);
        }
        let binding = latest
            .state
            .protocol
            .task_bindings
            .get(task_id)
            .ok_or(PersistenceError::InvalidSnapshot)?;
        let allocation = binding
            .allocation_certificate
            .as_ref()
            .map(|certificate| (binding.allocation.unwrap().0, certificate.clone()));
        let receipt = TaskReceipt::new(
            latest
                .generation
                .checked_add(1)
                .ok_or(PersistenceError::GenerationOverflow)?,
            plan.clone(),
            allocation,
        )?;
        // Both proofs are already authenticated in this exact loaded snapshot.
        // Cold decoding never takes this path and independently verifies them.
        let committee = super::snapshot_validation::resolve_validator_set(
            &latest.validator_set,
            &latest.retained_validator_sets,
            plan.validator_set_version,
        )
        .ok_or(PersistenceError::InvalidSnapshot)?;
        let allocation_committee = receipt
            .allocation
            .as_ref()
            .map(|(_, certificate)| {
                super::snapshot_validation::resolve_validator_set(
                    &latest.validator_set,
                    &latest.retained_validator_sets,
                    certificate.statement().validator_set_version(),
                )
                .cloned()
                .ok_or(PersistenceError::InvalidSnapshot)
            })
            .transpose()?;
        let _ = receipt
            .verified_for
            .set((committee.clone(), allocation_committee));
        receipts.insert(task_id.clone(), Arc::new(receipt));
    }
    let mut bytes = receipts
        .values()
        .map(|receipt| receipt.encoded.len())
        .sum::<usize>();
    while receipts.len() > MAX_TASK_RECEIPTS || bytes > MAX_TASK_RECEIPT_BYTES {
        let oldest = receipts
            .iter()
            .min_by_key(|(task_id, receipt)| (receipt.generation, *task_id))
            .map(|(task_id, _)| task_id.clone())
            .unwrap();
        bytes -= receipts.remove(&oldest).unwrap().encoded.len();
    }
    Ok(receipts)
}

pub(super) fn referenced_versions(receipts: &TaskReceipts) -> BTreeSet<u64> {
    receipts
        .values()
        .flat_map(|receipt| {
            std::iter::once(receipt.plan.validator_set_version).chain(
                receipt
                    .allocation
                    .as_ref()
                    .map(|(_, certificate)| certificate.statement().validator_set_version()),
            )
        })
        .collect()
}

pub(super) fn validate(
    generation: u64,
    state: &SecondState,
    active: &ValidatorSet,
    retained: &BTreeMap<u64, ValidatorSet>,
    receipts: &TaskReceipts,
) -> Result<(), PersistenceError> {
    if receipts.len() > MAX_TASK_RECEIPTS
        || receipts
            .values()
            .map(|receipt| receipt.encoded.len())
            .sum::<usize>()
            > MAX_TASK_RECEIPT_BYTES
    {
        return Err(PersistenceError::InvalidSnapshot);
    }
    for (task_id, receipt) in receipts {
        let plan = &receipt.plan;
        let binding = state
            .protocol
            .task_bindings
            .get(task_id)
            .ok_or(PersistenceError::InvalidSnapshot)?;
        if receipt.generation == 0
            || receipt.generation > generation
            || &plan.task_id != task_id
            || plan.phase != PreparedTaskPhase::Finalized
            || plan.commit_authorized
            || plan.conflict_abort
            || !plan.variants.is_empty()
            || state.task_succeeded(task_id.clone()) != Some(true)
            || binding.request_digest != plan.request_digest
            || plan.source_task.payload().task_id() != *task_id
        {
            return Err(PersistenceError::InvalidSnapshot);
        }
        let validators = super::snapshot_validation::resolve_validator_set(
            active,
            retained,
            plan.validator_set_version,
        )
        .ok_or(PersistenceError::InvalidSnapshot)?;
        let allocation_set = receipt
            .allocation
            .as_ref()
            .map(|(_, certificate)| {
                super::snapshot_validation::resolve_validator_set(
                    active,
                    retained,
                    certificate.statement().validator_set_version(),
                )
                .ok_or(PersistenceError::InvalidSnapshot)
            })
            .transpose()?;
        if let Some((start, certificate)) = &receipt.allocation {
            let (bound_start, count) = binding
                .allocation
                .ok_or(PersistenceError::InvalidSnapshot)?;
            if bound_start != *start
                || crate::CurrencyAllocation::digest_for(
                    allocation_set.unwrap().version(),
                    *start,
                    count,
                    plan.request_digest,
                ) != certificate.statement().subject_digest()
            {
                return Err(PersistenceError::InvalidSnapshot);
            }
        }
        if !receipt
            .verified_for
            .get()
            .is_some_and(|(committee, allocation)| {
                committee == validators && allocation.as_ref() == allocation_set
            })
        {
            if plan
                .source_task
                .request_digest()
                .map_err(|_| PersistenceError::InvalidSnapshot)?
                != plan.request_digest
            {
                return Err(PersistenceError::InvalidSnapshot);
            }
            let required = crate::currency_allocation::required_count_for_operations(
                plan.source_task.payload().operations(),
            )
            .map_err(|_| PersistenceError::InvalidSnapshot)?;
            if required != binding.allocation.map_or(0, |(_, count)| count)
                || (required > 0) != receipt.allocation.is_some()
            {
                return Err(PersistenceError::InvalidSnapshot);
            }
            receipt
                .certificate()?
                .verify(validators)
                .map_err(|_| PersistenceError::InvalidSnapshot)?;
            if let Some((_, certificate)) = &receipt.allocation {
                certificate
                    .verify(allocation_set.unwrap())
                    .map_err(|_| PersistenceError::InvalidSnapshot)?;
            }
            let _ = receipt
                .verified_for
                .set((validators.clone(), allocation_set.cloned()));
        }
    }
    Ok(())
}

pub(super) fn encode(out: &mut Vec<u8>, receipts: &TaskReceipts) -> Result<(), PersistenceError> {
    push_len(out, receipts.len())?;
    for receipt in receipts.values() {
        push_len(out, receipt.encoded.len())?;
        out.extend_from_slice(&receipt.encoded);
    }
    Ok(())
}

pub(super) fn decode(decoder: &mut Decoder<'_>) -> Result<TaskReceipts, PersistenceError> {
    let count = decoder.read_len()?;
    if count > MAX_TASK_RECEIPTS {
        return Err(PersistenceError::InvalidSnapshot);
    }
    let mut receipts = BTreeMap::new();
    let mut bytes = 0usize;
    for _ in 0..count {
        let length = decoder.read_len()?;
        bytes = bytes
            .checked_add(length)
            .ok_or(PersistenceError::InvalidSnapshot)?;
        if bytes > MAX_TASK_RECEIPT_BYTES {
            return Err(PersistenceError::InvalidSnapshot);
        }
        let encoded = decoder.read_exact(length)?;
        let mut inner = Decoder::new(encoded);
        let generation = inner.read_u64()?;
        let mut plans = decode_prepared_tasks(&mut inner)?;
        if plans.len() != 1 {
            return Err(PersistenceError::InvalidSnapshot);
        }
        let (task_id, plan) = plans.pop_first().unwrap();
        let allocation = match inner.read_u8()? {
            0 => None,
            1 => {
                let start = inner.read_u64()?;
                let size = inner.read_len()?;
                Some((
                    start,
                    crate::finality_codec::decode_certificate(inner.read_exact(size)?)
                        .ok_or(PersistenceError::InvalidSnapshot)?,
                ))
            }
            _ => return Err(PersistenceError::InvalidSnapshot),
        };
        inner.finish()?;
        let receipt = TaskReceipt::new(generation, plan, allocation)?;
        if receipt.encoded != encoded || receipts.insert(task_id, Arc::new(receipt)).is_some() {
            return Err(PersistenceError::InvalidSnapshot);
        }
    }
    Ok(receipts)
}
