use std::collections::{BTreeMap, BTreeSet};

use crate::{AccountAddress, PaymentAddress, PreparationError, TaskId};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum LifecycleAddress {
    Account(AccountAddress),
    Payment(PaymentAddress),
}

#[derive(Clone, Debug, Default)]
pub(crate) struct LifecycleClaimBook {
    claims: BTreeMap<LifecycleAddress, TaskId>,
}

impl LifecycleClaimBook {
    pub(crate) fn claim(
        &mut self,
        task_id: TaskId,
        address: LifecycleAddress,
    ) -> Result<(), PreparationError> {
        match self.claims.get(&address) {
            Some(owner) if owner != &task_id => Err(match address {
                LifecycleAddress::Account(account) => PreparationError::AccountContention(account),
                LifecycleAddress::Payment(payment) => {
                    PreparationError::PaymentAddressContention(payment)
                }
            }),
            Some(_) => Ok(()),
            None => {
                self.claims.insert(address, task_id);
                Ok(())
            }
        }
    }

    pub(crate) fn release_task(&mut self, task_id: &TaskId) {
        self.claims.retain(|_, owner| owner != task_id);
    }
    pub(crate) fn addresses(&self) -> impl Iterator<Item = LifecycleAddress> + '_ {
        self.claims.keys().copied()
    }

    pub(crate) fn conflicting_tasks(&self, candidate: &Self) -> BTreeSet<TaskId> {
        candidate
            .claims
            .iter()
            .filter_map(|(address, candidate_owner)| {
                self.claims
                    .get(address)
                    .and_then(|owner| (owner != candidate_owner).then(|| owner.clone()))
            })
            .collect()
    }
}
