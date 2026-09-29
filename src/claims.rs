use std::collections::{BTreeMap, BTreeSet};

use crate::state::BusinessState;
use crate::{AccountAddress, CurrencyAddress, CurrencyRole, ExecutionError, SecondState, TaskId};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
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

    pub const fn task_id(self) -> TaskId {
        self.task_id
    }

    pub const fn operation_index(self) -> u64 {
        self.operation_index
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConcurrentExecutionError {
    Execution(ExecutionError),
    Claim(ClaimError),
    OperationIndexOverflow,
}

impl From<ExecutionError> for ConcurrentExecutionError {
    fn from(error: ExecutionError) -> Self {
        Self::Execution(error)
    }
}

impl From<ClaimError> for ConcurrentExecutionError {
    fn from(error: ClaimError) -> Self {
        Self::Claim(error)
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
    claimed_currencies: Vec<CurrencyAddress>,
    selected_currencies: Vec<CurrencyAddress>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CurrencyClaimOwner {
    task_id: TaskId,
    references: u32,
}

#[derive(Clone, Debug, Default)]
pub struct CurrencyClaimBook {
    records: BTreeMap<OperationClaimId, ClaimRecord>,
    claimed_by_currency: BTreeMap<CurrencyAddress, CurrencyClaimOwner>,
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
    ) -> Result<Vec<CurrencyAddress>, ClaimError> {
        self.claim_transfer_in_business_state(&state.business, claim_id, source, amount)
    }

    pub(crate) fn claim_transfer_in_business_state(
        &mut self,
        business: &BusinessState,
        claim_id: OperationClaimId,
        source: AccountAddress,
        amount: u64,
    ) -> Result<Vec<CurrencyAddress>, ClaimError> {
        let requested_kind = ClaimKind::Transfer { source, amount };
        if let Some(existing) = self.existing_selection(claim_id, &requested_kind)? {
            return Ok(existing);
        }

        let mut total_owned = 0_u64;
        let mut available = Vec::new();

        for (address, currency) in &business.currencies {
            if currency.role != CurrencyRole::Circulation || currency.owner != Some(source) {
                continue;
            }

            total_owned += 1;
            if self.claimable_by_task(*address, claim_id.task_id()) {
                available.push(*address);
            }
        }

        if total_owned < amount {
            return Err(ClaimError::InsufficientBalance {
                account: source,
                required: amount,
                available: total_owned,
            });
        }

        let available_unclaimed = available.len() as u64;
        if available_unclaimed < amount {
            return Err(ClaimError::CurrencyContention {
                account: source,
                required: amount,
                available_unclaimed,
                claimed_elsewhere: total_owned.saturating_sub(available_unclaimed),
            });
        }

        let selected = available
            .into_iter()
            .take(amount as usize)
            .collect::<Vec<_>>();
        self.insert_claim(claim_id, requested_kind, selected.clone(), selected.clone());

        Ok(selected)
    }

    pub fn claim_reserve(
        &mut self,
        claim_id: OperationClaimId,
        state: &SecondState,
        count: u64,
    ) -> Result<Vec<CurrencyAddress>, ClaimError> {
        self.claim_reserve_in_business_state(claim_id, &state.business, count)
    }

    pub(crate) fn claim_reserve_in_business_state(
        &mut self,
        claim_id: OperationClaimId,
        business: &BusinessState,
        count: u64,
    ) -> Result<Vec<CurrencyAddress>, ClaimError> {
        let requested_kind = ClaimKind::Reserve { count };
        if let Some(existing) = self.existing_selection(claim_id, &requested_kind)? {
            return Ok(existing);
        }

        let selected = self.select_reserve(business, claim_id.task_id(), count)?;
        self.insert_claim(claim_id, requested_kind, selected.clone(), selected.clone());
        Ok(selected)
    }

    pub(crate) fn claim_explicit(
        &mut self,
        claim_id: OperationClaimId,
        currencies: &[CurrencyAddress],
    ) -> Result<Vec<CurrencyAddress>, ClaimError> {
        let requested_kind = ClaimKind::Explicit {
            currencies: currencies.to_vec(),
        };
        if let Some(existing) = self.existing_selection(claim_id, &requested_kind)? {
            return Ok(existing);
        }

        self.ensure_unique_explicit(currencies, claim_id)?;
        self.ensure_claimable(currencies, claim_id.task_id())?;
        let selected = currencies.to_vec();
        self.insert_claim(claim_id, requested_kind, selected.clone(), selected.clone());
        Ok(selected)
    }

    pub(crate) fn claim_leak_repair_in_business_state(
        &mut self,
        claim_id: OperationClaimId,
        business: &BusinessState,
        leaked: &[CurrencyAddress],
    ) -> Result<Vec<CurrencyAddress>, ClaimError> {
        let requested_kind = ClaimKind::LeakRepair {
            leaked: leaked.to_vec(),
            reserve_count: leaked.len() as u64,
        };
        if let Some(existing) = self.existing_selection(claim_id, &requested_kind)? {
            return Ok(existing);
        }

        self.ensure_unique_explicit(leaked, claim_id)?;
        self.ensure_claimable(leaked, claim_id.task_id())?;

        let reserve = self.select_reserve(business, claim_id.task_id(), leaked.len() as u64)?;
        let mut claimed = leaked.to_vec();
        claimed.extend_from_slice(&reserve);

        self.insert_claim(claim_id, requested_kind, claimed, reserve.clone());
        Ok(reserve)
    }

    pub fn release(&mut self, claim_id: OperationClaimId) {
        let Some(record) = self.records.remove(&claim_id) else {
            return;
        };

        for address in record.claimed_currencies {
            let should_remove = if let Some(owner) = self.claimed_by_currency.get_mut(&address) {
                if owner.task_id != claim_id.task_id() {
                    false
                } else if owner.references > 1 {
                    owner.references -= 1;
                    false
                } else {
                    true
                }
            } else {
                false
            };

            if should_remove {
                self.claimed_by_currency.remove(&address);
            }
        }
    }

    pub fn release_task(&mut self, task_id: TaskId) {
        let claims = self
            .records
            .keys()
            .copied()
            .filter(|claim_id| claim_id.task_id() == task_id)
            .collect::<Vec<_>>();

        for claim_id in claims {
            self.release(claim_id);
        }
    }

    pub fn claimed_currency_count(&self) -> usize {
        self.claimed_by_currency.len()
    }

    fn existing_selection(
        &self,
        claim_id: OperationClaimId,
        requested_kind: &ClaimKind,
    ) -> Result<Option<Vec<CurrencyAddress>>, ClaimError> {
        let Some(existing) = self.records.get(&claim_id) else {
            return Ok(None);
        };

        if &existing.kind == requested_kind {
            Ok(Some(existing.selected_currencies.clone()))
        } else {
            Err(ClaimError::ClaimIdentityConflict(claim_id))
        }
    }

    fn select_reserve(
        &self,
        business: &BusinessState,
        task_id: TaskId,
        count: u64,
    ) -> Result<Vec<CurrencyAddress>, ClaimError> {
        let mut total_reserve = 0_u64;
        let mut available = Vec::new();

        for (address, currency) in &business.currencies {
            if currency.role != CurrencyRole::Reserve || currency.owner.is_some() {
                continue;
            }

            total_reserve += 1;
            if self.claimable_by_task(*address, task_id) {
                available.push(*address);
            }
        }

        if total_reserve < count {
            return Err(ClaimError::ReserveUnavailable {
                required: count,
                available: total_reserve,
            });
        }

        let available_unclaimed = available.len() as u64;
        if available_unclaimed < count {
            return Err(ClaimError::ReserveContention {
                required: count,
                available_unclaimed,
                claimed_elsewhere: total_reserve.saturating_sub(available_unclaimed),
            });
        }

        Ok(available
            .into_iter()
            .take(count as usize)
            .collect::<Vec<_>>())
    }

    fn ensure_claimable(
        &self,
        currencies: &[CurrencyAddress],
        task_id: TaskId,
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
        claim_id: OperationClaimId,
    ) -> Result<(), ClaimError> {
        let mut seen = BTreeSet::new();
        for address in currencies {
            if !seen.insert(*address) {
                return Err(ClaimError::ClaimIdentityConflict(claim_id));
            }
        }
        Ok(())
    }

    fn claimable_by_task(&self, address: CurrencyAddress, task_id: TaskId) -> bool {
        self.claimed_by_currency
            .get(&address)
            .is_none_or(|owner| owner.task_id == task_id)
    }

    fn insert_claim(
        &mut self,
        claim_id: OperationClaimId,
        kind: ClaimKind,
        claimed_currencies: Vec<CurrencyAddress>,
        selected_currencies: Vec<CurrencyAddress>,
    ) {
        for address in &claimed_currencies {
            match self.claimed_by_currency.get_mut(address) {
                Some(owner) if owner.task_id == claim_id.task_id() => {
                    owner.references = owner.references.saturating_add(1);
                }
                Some(_) => {
                    unreachable!("different task claim must be excluded before insertion");
                }
                None => {
                    self.claimed_by_currency.insert(
                        *address,
                        CurrencyClaimOwner {
                            task_id: claim_id.task_id(),
                            references: 1,
                        },
                    );
                }
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
    }
}
