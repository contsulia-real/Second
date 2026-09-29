use std::collections::{BTreeMap, BTreeSet};

use crate::currency::Currency;
use crate::{
    AccountAddress, CurrencyAddress, CurrencyRole, ExecutionError, NetworkError,
    PublicCurrencyPage, PublicCurrencyState, TaskId,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionOutcome {
    Succeeded,
    AlreadySucceeded,
}

#[derive(Clone, Debug)]
pub(crate) struct TaskBinding {
    pub(crate) request_digest: [u8; 32],
    pub(crate) succeeded: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct ProtocolState {
    pub(crate) next_currency_address: u64,
    pub(crate) task_bindings: BTreeMap<TaskId, TaskBinding>,
}

#[derive(Clone, Debug)]
pub(crate) struct BusinessState {
    pub(crate) accounts: BTreeSet<AccountAddress>,
    pub(crate) currencies: BTreeMap<CurrencyAddress, Currency>,
}

#[derive(Clone)]
pub struct SecondState {
    pub(crate) protocol: ProtocolState,
    pub(crate) business: BusinessState,
}

impl SecondState {
    pub fn genesis<I>(accounts: I, first_currency_address: u64) -> Self
    where
        I: IntoIterator<Item = AccountAddress>,
    {
        Self {
            protocol: ProtocolState {
                next_currency_address: first_currency_address,
                task_bindings: BTreeMap::new(),
            },
            business: BusinessState {
                accounts: accounts.into_iter().collect(),
                currencies: BTreeMap::new(),
            },
        }
    }

    pub fn with_reserve(mut self, count: u64) -> Result<Self, ExecutionError> {
        let range = self.allocate_currency_range(count)?;
        for address in range {
            self.business.currencies.insert(
                address,
                Currency {
                    address,
                    role: CurrencyRole::Reserve,
                    owner: None,
                },
            );
        }
        Ok(self)
    }

    pub fn balance(&self, account: AccountAddress) -> u64 {
        self.business
            .currencies
            .values()
            .filter(|currency| currency.owner == Some(account))
            .count() as u64
    }

    pub fn current_supply(&self) -> u64 {
        self.business.currencies.len() as u64
    }

    pub fn reserve_count(&self) -> u64 {
        self.business
            .currencies
            .values()
            .filter(|currency| currency.role == CurrencyRole::Reserve && currency.owner.is_none())
            .count() as u64
    }

    pub const fn next_currency_address(&self) -> u64 {
        self.protocol.next_currency_address
    }

    pub fn currency_exists(&self, address: CurrencyAddress) -> bool {
        self.business.currencies.contains_key(&address)
    }

    pub fn public_currency_state(&self, address: CurrencyAddress) -> Option<PublicCurrencyState> {
        self.business
            .currencies
            .get(&address)
            .map(Currency::public_state)
    }

    pub fn public_currency_summary(&self) -> crate::PublicCurrencySummary {
        crate::public_state::summarize_public_currency_state(self)
    }

    pub fn public_currency_states(&self) -> Vec<PublicCurrencyState> {
        self.business
            .currencies
            .values()
            .map(Currency::public_state)
            .collect()
    }

    pub fn public_currency_page(
        &self,
        start: CurrencyAddress,
        limit: u16,
    ) -> Result<PublicCurrencyPage, NetworkError> {
        crate::network::validate_public_currency_limit(limit)?;

        let mut currencies = self.business.currencies.range(start..);
        let states = currencies
            .by_ref()
            .take(usize::from(limit))
            .map(|(_, currency)| currency.public_state())
            .collect::<Vec<_>>();
        let next_start = currencies.next().map(|(address, _)| *address);

        Ok(PublicCurrencyPage { states, next_start })
    }

    pub fn bound_request_digest(&self, task_id: TaskId) -> Option<[u8; 32]> {
        self.protocol
            .task_bindings
            .get(&task_id)
            .map(|binding| binding.request_digest)
    }

    pub(crate) fn allocate_currency_range(
        &mut self,
        count: u64,
    ) -> Result<Vec<CurrencyAddress>, ExecutionError> {
        if count == 0 {
            return Ok(Vec::new());
        }

        let start = self.protocol.next_currency_address;
        let end_exclusive = start
            .checked_add(count)
            .ok_or(ExecutionError::IdentitySpaceExhausted)?;

        self.protocol.next_currency_address = end_exclusive;

        Ok((start..end_exclusive).map(CurrencyAddress::new).collect())
    }
}
