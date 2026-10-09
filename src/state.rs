use std::collections::{BTreeMap, BTreeSet};

use crate::currency::Currency;
use crate::payment::{PaymentAddressRecord, PaymentExecution};
use crate::{
    AccountAddress, CurrencyAddress, CurrencyRole, ExecutionError, FinalityStatement, NetworkError,
    OperationClaimId, PaymentAddress, PublicCurrencyPage, PublicCurrencyState, TaskId,
    VerifiedLegalTask,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionOutcome {
    Succeeded,
    AlreadySucceeded,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TaskOutcome {
    Pending,
    Succeeded,
    Cancelled(FinalityStatement),
}

impl TaskOutcome {
    pub(crate) fn is_terminal(&self) -> bool {
        !matches!(self, Self::Pending)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TaskBinding {
    pub(crate) request_digest: [u8; 32],
    pub(crate) outcome: TaskOutcome,
    pub(crate) allocation: Option<(u64, u64)>,
    // Original signed request awaiting allocation/preparation, including retry
    // after a quorum-certified committee cut retires unprotected local rights.
    pub(crate) allocation_task: Option<crate::LegalTask>,
    pub(crate) allocation_certificate: Option<crate::FinalityCertificate>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProtocolState {
    pub(crate) next_currency_address: u64,
    pub(crate) task_bindings: BTreeMap<TaskId, TaskBinding>,
    pub(crate) task_handoff: Option<std::sync::Arc<crate::persistence::TaskHandoff>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct PrerequisiteState {
    pub(crate) payment_executions: BTreeMap<OperationClaimId, PaymentExecution>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BusinessState {
    pub(crate) accounts: BTreeSet<AccountAddress>,
    pub(crate) payment_addresses: BTreeMap<PaymentAddress, PaymentAddressRecord>,
    pub(crate) currencies: BTreeMap<CurrencyAddress, Currency>,
    pub(crate) payment_history: BTreeMap<OperationClaimId, PaymentExecution>,
}

#[derive(Clone)]
pub struct SecondState {
    pub(crate) protocol: ProtocolState,
    pub(crate) prerequisite: PrerequisiteState,
    pub(crate) business: BusinessState,
}

impl SecondState {
    pub(crate) fn same_persisted_state(&self, other: &Self) -> bool {
        self.protocol == other.protocol
            && self.prerequisite == other.prerequisite
            && self.business == other.business
    }

    pub fn genesis<I>(accounts: I, first_currency_address: u64) -> Self
    where
        I: IntoIterator<Item = AccountAddress>,
    {
        Self {
            protocol: ProtocolState {
                next_currency_address: first_currency_address,
                task_bindings: BTreeMap::new(),
                task_handoff: None,
            },
            prerequisite: PrerequisiteState::default(),
            business: BusinessState {
                accounts: accounts.into_iter().collect(),
                payment_addresses: BTreeMap::new(),
                currencies: BTreeMap::new(),
                payment_history: BTreeMap::new(),
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

    pub(crate) fn bind_task(&mut self, task: &VerifiedLegalTask) -> Result<bool, ExecutionError> {
        self.bind_task_request_digest(task.task_id(), task.request_digest())
    }

    pub(crate) fn bind_task_request_digest(
        &mut self,
        task_id: TaskId,
        request_digest: [u8; 32],
    ) -> Result<bool, ExecutionError> {
        match self.protocol.task_bindings.get(&task_id) {
            Some(binding) if binding.request_digest != request_digest => {
                Err(ExecutionError::TaskIdAlreadyBound)
            }
            Some(binding) => match binding.outcome {
                TaskOutcome::Pending => Ok(false),
                TaskOutcome::Succeeded => Ok(true),
                TaskOutcome::Cancelled(_) => Err(ExecutionError::TaskCancelled),
            },
            None => {
                self.protocol.task_bindings.insert(
                    task_id,
                    TaskBinding {
                        request_digest,
                        outcome: TaskOutcome::Pending,
                        allocation: None,
                        allocation_task: None,
                        allocation_certificate: None,
                    },
                );
                Ok(false)
            }
        }
    }

    pub(crate) fn mark_task_succeeded(&mut self, task_id: TaskId) {
        if let Some(binding) = self.protocol.task_bindings.get_mut(&task_id) {
            binding.outcome = TaskOutcome::Succeeded;
            binding.allocation_certificate = None;
            binding.allocation_task = None;
        }
    }

    pub fn bound_request_digest(&self, task_id: TaskId) -> Option<[u8; 32]> {
        self.protocol
            .task_bindings
            .get(&task_id)
            .map(|binding| binding.request_digest)
    }
    pub fn task_cancelled(&self, task_id: TaskId) -> bool {
        self.protocol
            .task_bindings
            .get(&task_id)
            .is_some_and(|binding| matches!(binding.outcome, TaskOutcome::Cancelled(_)))
    }

    pub fn task_succeeded(&self, task_id: TaskId) -> Option<bool> {
        self.protocol
            .task_bindings
            .get(&task_id)
            .map(|binding| matches!(binding.outcome, TaskOutcome::Succeeded))
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
            .ok_or(ExecutionError::CurrencySequenceSpaceExhausted)?;
        let addresses = Self::materialize_addresses(start, count)?;
        self.protocol.next_currency_address = end_exclusive;
        Ok(addresses)
    }

    pub(crate) fn materialize_addresses(
        start: u64,
        count: u64,
    ) -> Result<Vec<CurrencyAddress>, ExecutionError> {
        let end_exclusive = start
            .checked_add(count)
            .ok_or(ExecutionError::CurrencySequenceSpaceExhausted)?;
        let mut addresses = Self::reserve_address_buffer(count)?;
        addresses.extend((start..end_exclusive).map(CurrencyAddress::new));
        Ok(addresses)
    }

    pub(crate) fn reserve_address_buffer(
        count: u64,
    ) -> Result<Vec<CurrencyAddress>, ExecutionError> {
        let capacity = usize::try_from(count)
            .map_err(|_| ExecutionError::CurrencyAllocationFailed { requested: count })?;
        let mut addresses = Vec::new();
        addresses
            .try_reserve_exact(capacity)
            .map_err(|_| ExecutionError::CurrencyAllocationFailed { requested: count })?;
        Ok(addresses)
    }

    pub(crate) fn allocated_task_addresses(
        &self,
        task: &VerifiedLegalTask,
        offset: u64,
        count: u64,
    ) -> Result<Vec<CurrencyAddress>, ExecutionError> {
        let (start, total) = self
            .protocol
            .task_bindings
            .get(&task.task_id())
            .filter(|binding| binding.request_digest == task.request_digest())
            .and_then(|binding| binding.allocation)
            .ok_or(ExecutionError::CurrencyAllocationRequired)?;
        if offset.checked_add(count).is_none_or(|end| end > total) {
            return Err(ExecutionError::CurrencyAllocationRequired);
        }
        Self::materialize_addresses(start + offset, count)
    }
}
