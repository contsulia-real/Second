use std::collections::BTreeMap;

use crate::{AccountAddress, CurrencyAddress, CurrencyRole, SecondState, TaskId};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct OperationClaimId {
    task_id: TaskId,
    operation_index: u32,
}

impl OperationClaimId {
    pub const fn new(task_id: TaskId, operation_index: u32) -> Self {
        Self {
            task_id,
            operation_index,
        }
    }

    pub const fn task_id(self) -> TaskId {
        self.task_id
    }

    pub const fn operation_index(self) -> u32 {
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
    Transfer { source: AccountAddress, amount: u64 },
    Reserve { count: u64 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ClaimRecord {
    kind: ClaimKind,
    currencies: Vec<CurrencyAddress>,
}

#[derive(Clone, Debug, Default)]
pub struct CurrencyClaimBook {
    records: BTreeMap<OperationClaimId, ClaimRecord>,
    claimed_by_currency: BTreeMap<CurrencyAddress, OperationClaimId>,
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
        let requested_kind = ClaimKind::Transfer { source, amount };
        if let Some(existing) = self.records.get(&claim_id) {
            return if existing.kind == requested_kind {
                Ok(existing.currencies.clone())
            } else {
                Err(ClaimError::ClaimIdentityConflict(claim_id))
            };
        }

        let mut total_owned = 0_u64;
        let mut available = Vec::new();

        for (address, currency) in &state.business.currencies {
            if currency.role != CurrencyRole::Circulation || currency.owner != Some(source) {
                continue;
            }

            total_owned += 1;
            if !self.claimed_by_currency.contains_key(address) {
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

        let currencies = available
            .into_iter()
            .take(amount as usize)
            .collect::<Vec<_>>();
        self.insert_claim(claim_id, requested_kind, currencies.clone());

        Ok(currencies)
    }

    pub fn claim_reserve(
        &mut self,
        claim_id: OperationClaimId,
        state: &SecondState,
        count: u64,
    ) -> Result<Vec<CurrencyAddress>, ClaimError> {
        let requested_kind = ClaimKind::Reserve { count };
        if let Some(existing) = self.records.get(&claim_id) {
            return if existing.kind == requested_kind {
                Ok(existing.currencies.clone())
            } else {
                Err(ClaimError::ClaimIdentityConflict(claim_id))
            };
        }

        let mut total_reserve = 0_u64;
        let mut available = Vec::new();

        for (address, currency) in &state.business.currencies {
            if currency.role != CurrencyRole::Reserve || currency.owner.is_some() {
                continue;
            }

            total_reserve += 1;
            if !self.claimed_by_currency.contains_key(address) {
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

        let currencies = available
            .into_iter()
            .take(count as usize)
            .collect::<Vec<_>>();
        self.insert_claim(claim_id, requested_kind, currencies.clone());

        Ok(currencies)
    }

    pub fn release(&mut self, claim_id: OperationClaimId) {
        let Some(record) = self.records.remove(&claim_id) else {
            return;
        };

        for address in record.currencies {
            if self.claimed_by_currency.get(&address) == Some(&claim_id) {
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

    fn insert_claim(
        &mut self,
        claim_id: OperationClaimId,
        kind: ClaimKind,
        currencies: Vec<CurrencyAddress>,
    ) {
        for address in &currencies {
            self.claimed_by_currency.insert(*address, claim_id);
        }

        self.records
            .insert(claim_id, ClaimRecord { kind, currencies });
    }
}
