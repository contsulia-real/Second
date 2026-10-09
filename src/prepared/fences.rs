//! Derived indexes for certified handoff obligations; never business ownership.
use super::lifecycle::{LifecycleAddress, LifecycleClaimBook};
use crate::prepared_plan::{PreparedOperation, PreparedTask};
use crate::{
    CurrencyAddress, CurrencyClaimBook, PaymentAddress, PreparationError, SecondState, TaskId,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default)]
pub(crate) struct ResourceFenceIndex {
    currency: BTreeMap<CurrencyAddress, BTreeSet<TaskId>>,
    lifecycle: BTreeMap<LifecycleAddress, BTreeSet<TaskId>>,
    executions: BTreeMap<PaymentAddress, BTreeSet<TaskId>>,
}

impl ResourceFenceIndex {
    pub(crate) fn new<'a>(
        plans: impl Iterator<Item = &'a PreparedTask>,
    ) -> Result<Self, PreparationError> {
        Self::build(plans, false, false)
    }

    pub(crate) fn owned<'a>(
        plans: impl Iterator<Item = &'a PreparedTask>,
    ) -> Result<Self, PreparationError> {
        Self::build(plans, true, false)
    }

    pub(crate) fn for_handoff<'a>(
        plans: impl Iterator<Item = &'a PreparedTask>,
    ) -> Result<Self, PreparationError> {
        Self::build(plans, false, true)
    }

    fn build<'a>(
        plans: impl Iterator<Item = &'a PreparedTask>,
        owned_only: bool,
        inherited_executions: bool,
    ) -> Result<Self, PreparationError> {
        let mut index = Self::default();
        for plan in plans {
            let mut currencies = CurrencyClaimBook::new();
            let mut lifecycle = LifecycleClaimBook::default();
            plan.restore_candidate_claims(&mut currencies, owned_only)?;
            plan.restore_candidate_lifecycle_claims(&mut lifecycle, owned_only)?;
            for address in currencies.claimed_addresses() {
                index
                    .currency
                    .entry(address)
                    .or_default()
                    .insert(plan.task_id.clone());
            }
            for address in lifecycle.addresses() {
                index
                    .lifecycle
                    .entry(address)
                    .or_default()
                    .insert(plan.task_id.clone());
            }
            if inherited_executions {
                // Handoff plans are canonical single candidates. This index is
                // cached once with the immutable body, not rebuilt for ordinary
                // task contention or local owned-claim queries.
                for operation in &plan.operations {
                    if let PreparedOperation::Transfer { transfer, .. } = operation {
                        for address in [transfer.source, transfer.destination] {
                            index
                                .executions
                                .entry(address)
                                .or_default()
                                .insert(plan.task_id.clone());
                        }
                    }
                }
            }
        }
        Ok(index)
    }

    pub(crate) fn blockers(
        &self,
        state: &SecondState,
        candidate: &PreparedTask,
    ) -> Result<BTreeSet<TaskId>, PreparationError> {
        let mut currencies = CurrencyClaimBook::new();
        let mut lifecycle = LifecycleClaimBook::default();
        candidate.restore_claims(&mut currencies)?;
        candidate.restore_lifecycle_claims(&mut lifecycle)?;
        let mut blocked = BTreeSet::new();
        for owners in currencies
            .claimed_addresses()
            .filter_map(|address| self.currency.get(&address))
            .chain(
                lifecycle
                    .addresses()
                    .filter_map(|address| self.lifecycle.get(&address)),
            )
        {
            for owner in owners {
                if owner != &candidate.task_id
                    && !state
                        .protocol
                        .task_bindings
                        .get(owner)
                        .is_some_and(|binding| binding.outcome.is_terminal())
                {
                    blocked.insert(owner.clone());
                }
            }
        }
        Ok(blocked)
    }

    /// Only authenticated inherited executions prevent final retirement.
    /// Starting retirement remains allowed, as with local established transfers.
    pub(crate) fn inherited_execution_blockers(
        &self,
        state: &SecondState,
        candidate: &PreparedTask,
    ) -> BTreeSet<TaskId> {
        candidate
            .operations
            .iter()
            .filter_map(|operation| match operation {
                PreparedOperation::FinalizePaymentAddressRetirement { address } => {
                    self.executions.get(address)
                }
                _ => None,
            })
            .flatten()
            .filter(|task| {
                **task != candidate.task_id
                    && !state
                        .protocol
                        .task_bindings
                        .get(*task)
                        .is_some_and(|binding| binding.outcome.is_terminal())
            })
            .cloned()
            .collect()
    }
}
