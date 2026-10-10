use std::collections::{BTreeMap, BTreeSet};

use crate::range_map::RangeMap;
use crate::state::BusinessState;
use crate::{AccountAddress, CurrencyAddress, CurrencyRole, SecondState, TaskId};
use crate::{AddressRange, AddressRanges};

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct OperationClaimId {
    task_id: TaskId,
    operation_index: u64,
}

impl OperationClaimId {
    pub const fn new(task_id: TaskId, operation_index: u64) -> Self {
        Self {
            task_id,
            operation_index,
        }
    }

    pub fn task_id(&self) -> &TaskId {
        &self.task_id
    }

    pub const fn operation_index(&self) -> u64 {
        self.operation_index
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClaimError {
    ClaimIdentityConflict(OperationClaimId),
    InsufficientBalance {
        account: AccountAddress,
        required: u64,
        available: u64,
    },
    CurrencyContention {
        account: AccountAddress,
        required: u64,
        available_unclaimed: u64,
        claimed_elsewhere: u64,
    },
    ExplicitCurrencyContention {
        currency: CurrencyAddress,
    },
    ClaimReferenceOverflow(CurrencyAddress),
    ReserveUnavailable {
        required: u64,
        available: u64,
    },
    ReserveContention {
        required: u64,
        available_unclaimed: u64,
        claimed_elsewhere: u64,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ClaimKind {
    Transfer {
        source: AccountAddress,
        amount: u64,
    },
    Reserve {
        count: u64,
    },
    Explicit {
        currencies: Vec<CurrencyAddress>,
    },
    LeakRepair {
        leaked: Vec<CurrencyAddress>,
        reserve_count: u64,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ClaimRecord {
    kind: ClaimKind,
    claimed_currencies: AddressRanges,
    selected_currencies: AddressRanges,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CurrencyClaimOwner {
    task_id: TaskId,
    references: u32,
}

#[derive(Clone, Debug, Default)]
pub struct CurrencyClaimBook {
    records: BTreeMap<OperationClaimId, ClaimRecord>,
    claimed_by_currency: RangeMap<CurrencyClaimOwner>,
}

impl CurrencyClaimBook {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn claim_transfer(
        &mut self,
        state: &SecondState,
        claim_id: OperationClaimId,
        source: AccountAddress,
        amount: u64,
    ) -> Result<AddressRanges, ClaimError> {
        self.claim_transfer_in_business_state(&state.business, claim_id, source, amount)
    }

    pub(crate) fn claim_transfer_in_business_state(
        &mut self,
        business: &BusinessState,
        claim_id: OperationClaimId,
        source: AccountAddress,
        amount: u64,
    ) -> Result<AddressRanges, ClaimError> {
        let requested_kind = ClaimKind::Transfer { source, amount };
        if let Some(existing) = self.existing_selection(&claim_id, &requested_kind)? {
            return Ok(existing);
        }

        let mut owned = AddressRanges::default();
        for (start, run) in business.currencies.owned_runs(source) {
            if run.role == CurrencyRole::Circulation {
                owned.insert(AddressRange::new(start, run.len).unwrap());
            }
        }
        let total_owned = owned.len();
        let available = self.available(&owned, claim_id.task_id());

        if total_owned < amount {
            return Err(ClaimError::InsufficientBalance {
                account: source,
                required: amount,
                available: total_owned,
            });
        }

        let available_unclaimed = available.len();
        if available_unclaimed < amount {
            return Err(ClaimError::CurrencyContention {
                account: source,
                required: amount,
                available_unclaimed,
                claimed_elsewhere: total_owned.saturating_sub(available_unclaimed),
            });
        }

        let selected = available.take(amount);
        self.insert_claim(claim_id, requested_kind, selected.clone(), selected.clone())?;

        Ok(selected)
    }

    pub fn claim_reserve(
        &mut self,
        claim_id: OperationClaimId,
        state: &SecondState,
        count: u64,
    ) -> Result<AddressRanges, ClaimError> {
        self.claim_reserve_in_business_state(claim_id, &state.business, count)
    }

    pub(crate) fn claim_reserve_in_business_state(
        &mut self,
        claim_id: OperationClaimId,
        business: &BusinessState,
        count: u64,
    ) -> Result<AddressRanges, ClaimError> {
        let requested_kind = ClaimKind::Reserve { count };
        if let Some(existing) = self.existing_selection(&claim_id, &requested_kind)? {
            return Ok(existing);
        }

        let selected = self.select_reserve(business, claim_id.task_id(), count)?;
        self.insert_claim(claim_id, requested_kind, selected.clone(), selected.clone())?;
        Ok(selected)
    }

    pub(crate) fn claim_explicit(
        &mut self,
        claim_id: OperationClaimId,
        currencies: &[CurrencyAddress],
    ) -> Result<AddressRanges, ClaimError> {
        let requested_kind = ClaimKind::Explicit {
            currencies: currencies.to_vec(),
        };
        if let Some(existing) = self.existing_selection(&claim_id, &requested_kind)? {
            return Ok(existing);
        }

        self.ensure_unique_explicit(currencies, &claim_id)?;
        self.ensure_claimable(currencies, claim_id.task_id())?;
        let selected = currencies.iter().copied().collect::<AddressRanges>();
        self.insert_claim(claim_id, requested_kind, selected.clone(), selected.clone())?;
        Ok(selected)
    }

    pub(crate) fn claim_leak_repair_in_business_state(
        &mut self,
        claim_id: OperationClaimId,
        business: &BusinessState,
        leaked: &[CurrencyAddress],
    ) -> Result<AddressRanges, ClaimError> {
        let requested_kind = ClaimKind::LeakRepair {
            leaked: leaked.to_vec(),
            reserve_count: leaked.len() as u64,
        };
        if let Some(existing) = self.existing_selection(&claim_id, &requested_kind)? {
            return Ok(existing);
        }

        self.ensure_unique_explicit(leaked, &claim_id)?;
        self.ensure_claimable(leaked, claim_id.task_id())?;

        let reserve = self.select_reserve(business, claim_id.task_id(), leaked.len() as u64)?;
        let mut claimed = leaked.iter().copied().collect::<AddressRanges>();
        claimed.union_with(&reserve);

        self.insert_claim(claim_id, requested_kind, claimed, reserve.clone())?;
        Ok(reserve)
    }

    pub fn release(&mut self, claim_id: OperationClaimId) {
        let Some(record) = self.records.remove(&claim_id) else {
            return;
        };

        for range in record.claimed_currencies.ranges() {
            for (part, mut owner) in self.claimed_by_currency.overlapping(*range) {
                if &owner.task_id != claim_id.task_id() {
                    continue;
                }
                owner.references -= 1;
                let value = (owner.references > 0).then_some(owner);
                self.claimed_by_currency.set(part, value);
            }
        }
    }

    pub fn release_task(&mut self, task_id: TaskId) {
        let claims = self
            .records
            .keys()
            .filter(|claim_id| claim_id.task_id() == &task_id)
            .cloned()
            .collect::<Vec<_>>();

        for claim_id in claims {
            self.release(claim_id);
        }
    }

    pub fn claimed_currency_count(&self) -> u64 {
        self.claimed_by_currency
            .runs()
            .map(|(range, _)| range.len)
            .sum()
    }
    pub(crate) fn claimed_ranges(&self) -> AddressRanges {
        let mut ranges = AddressRanges::default();
        for (range, _) in self.claimed_by_currency.runs() {
            ranges.insert(range);
        }
        ranges
    }
    pub(crate) fn conflicting_tasks(&self, candidate: &Self) -> BTreeSet<TaskId> {
        let mut tasks = BTreeSet::new();
        for (range, candidate_owner) in candidate.claimed_by_currency.runs() {
            for (_, owner) in self.claimed_by_currency.overlapping(range) {
                if owner.task_id != candidate_owner.task_id {
                    tasks.insert(owner.task_id);
                }
            }
        }
        tasks
    }

    pub(crate) fn restore_transfer(
        &mut self,
        claim_id: OperationClaimId,
        source: AccountAddress,
        currencies: &AddressRanges,
    ) -> Result<(), ClaimError> {
        self.ensure_ranges_claimable(currencies, claim_id.task_id())?;
        self.insert_claim(
            claim_id,
            ClaimKind::Transfer {
                source,
                amount: currencies.len(),
            },
            currencies.clone(),
            currencies.clone(),
        )
    }

    pub(crate) fn restore_explicit(
        &mut self,
        claim_id: OperationClaimId,
        currencies: &[CurrencyAddress],
    ) -> Result<(), ClaimError> {
        self.ensure_unique_explicit(currencies, &claim_id)?;
        self.ensure_claimable(currencies, claim_id.task_id())?;
        self.insert_claim(
            claim_id,
            ClaimKind::Explicit {
                currencies: currencies.to_vec(),
            },
            currencies.iter().copied().collect(),
            currencies.iter().copied().collect(),
        )
    }

    pub(crate) fn restore_leak_repair(
        &mut self,
        claim_id: OperationClaimId,
        leaked: &[CurrencyAddress],
        reserve: &AddressRanges,
    ) -> Result<(), ClaimError> {
        self.ensure_unique_explicit(leaked, &claim_id)?;
        let leaked_ranges = leaked.iter().copied().collect::<AddressRanges>();
        if leaked_ranges.intersects(reserve) {
            return Err(ClaimError::ClaimIdentityConflict(claim_id));
        }
        let mut claimed = leaked_ranges;
        claimed.union_with(reserve);
        self.ensure_ranges_claimable(&claimed, claim_id.task_id())?;
        self.insert_claim(
            claim_id,
            ClaimKind::LeakRepair {
                leaked: leaked.to_vec(),
                reserve_count: reserve.len(),
            },
            claimed,
            reserve.clone(),
        )
    }

    fn existing_selection(
        &self,
        claim_id: &OperationClaimId,
        requested_kind: &ClaimKind,
    ) -> Result<Option<AddressRanges>, ClaimError> {
        let Some(existing) = self.records.get(claim_id) else {
            return Ok(None);
        };

        if &existing.kind == requested_kind {
            Ok(Some(existing.selected_currencies.clone()))
        } else {
            Err(ClaimError::ClaimIdentityConflict(claim_id.clone()))
        }
    }

    fn select_reserve(
        &self,
        business: &BusinessState,
        task_id: &TaskId,
        count: u64,
    ) -> Result<AddressRanges, ClaimError> {
        let mut reserves = AddressRanges::default();
        for (start, run) in business.currencies.reserve_runs() {
            reserves.insert(AddressRange::new(start, run.len).unwrap());
        }
        let total_reserve = reserves.len();
        let available = self.available(&reserves, task_id);

        if total_reserve < count {
            return Err(ClaimError::ReserveUnavailable {
                required: count,
                available: total_reserve,
            });
        }

        let available_unclaimed = available.len();
        if available_unclaimed < count {
            return Err(ClaimError::ReserveContention {
                required: count,
                available_unclaimed,
                claimed_elsewhere: total_reserve.saturating_sub(available_unclaimed),
            });
        }

        Ok(available.take(count))
    }

    fn available(&self, ranges: &AddressRanges, task: &TaskId) -> AddressRanges {
        let mut blocked = AddressRanges::default();
        for (range, owner) in self.claimed_by_currency.runs() {
            if &owner.task_id != task {
                blocked.insert(range);
            }
        }
        ranges.difference(&blocked)
    }

    fn ensure_ranges_claimable(
        &self,
        ranges: &AddressRanges,
        task: &TaskId,
    ) -> Result<(), ClaimError> {
        for range in ranges.ranges() {
            for (part, owner) in self.claimed_by_currency.overlapping(*range) {
                if &owner.task_id != task {
                    return Err(ClaimError::ExplicitCurrencyContention {
                        currency: part.start,
                    });
                }
            }
        }
        Ok(())
    }

    fn ensure_claimable(
        &self,
        currencies: &[CurrencyAddress],
        task_id: &TaskId,
    ) -> Result<(), ClaimError> {
        for address in currencies {
            if !self.claimable_by_task(*address, task_id) {
                return Err(ClaimError::ExplicitCurrencyContention { currency: *address });
            }
        }
        Ok(())
    }

    fn ensure_unique_explicit(
        &self,
        currencies: &[CurrencyAddress],
        claim_id: &OperationClaimId,
    ) -> Result<(), ClaimError> {
        let mut seen = BTreeSet::new();
        for address in currencies {
            if AddressRange::new(*address, 1).is_none() || !seen.insert(*address) {
                return Err(ClaimError::ClaimIdentityConflict(claim_id.clone()));
            }
        }
        Ok(())
    }

    fn claimable_by_task(&self, address: CurrencyAddress, task_id: &TaskId) -> bool {
        self.claimed_by_currency
            .get(address)
            .is_none_or(|owner| &owner.task_id == task_id)
    }

    fn insert_claim(
        &mut self,
        claim_id: OperationClaimId,
        kind: ClaimKind,
        claimed_currencies: AddressRanges,
        selected_currencies: AddressRanges,
    ) -> Result<(), ClaimError> {
        self.ensure_ranges_claimable(&claimed_currencies, claim_id.task_id())?;
        for range in claimed_currencies.ranges() {
            for (part, owner) in self.claimed_by_currency.overlapping(*range) {
                if owner.references == u32::MAX {
                    return Err(ClaimError::ClaimReferenceOverflow(part.start));
                }
            }
        }
        for range in claimed_currencies.ranges() {
            let existing = self.claimed_by_currency.overlapping(*range);
            self.claimed_by_currency.set(
                *range,
                Some(CurrencyClaimOwner {
                    task_id: claim_id.task_id().clone(),
                    references: 1,
                }),
            );
            for (part, mut owner) in existing {
                owner.references += 1;
                self.claimed_by_currency.set(part, Some(owner));
            }
        }

        self.records.insert(
            claim_id,
            ClaimRecord {
                kind,
                claimed_currencies,
                selected_currencies,
            },
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests;
