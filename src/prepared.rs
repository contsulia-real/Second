use std::collections::BTreeMap;

use crate::state::BusinessState;
use crate::{
    AccountAddress, ClaimError, CurrencyAddress, CurrencyClaimBook, ExecutionError,
    ExecutionOutcome, Operation, OperationClaimId, PersistenceError, SecondState, StateStore,
    TaskId, ValidatorSet, VerifiedLegalTask,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreparationOutcome {
    Prepared,
    AlreadySucceeded,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PreparationError {
    Execution(ExecutionError),
    Claim(ClaimError),
    Persistence(PersistenceError),
    AlreadyPrepared(TaskId),
    NotPrepared(TaskId),
    OperationIndexOverflow,
    LengthOverflow,
}

impl From<ExecutionError> for PreparationError {
    fn from(error: ExecutionError) -> Self {
        Self::Execution(error)
    }
}

impl From<ClaimError> for PreparationError {
    fn from(error: ClaimError) -> Self {
        Self::Claim(error)
    }
}

impl From<PersistenceError> for PreparationError {
    fn from(error: PersistenceError) -> Self {
        Self::Persistence(error)
    }
}

#[derive(Clone, Debug)]
enum PreparedOperation {
    Issue {
        account: AccountAddress,
        addresses: Vec<CurrencyAddress>,
    },
    Transfer {
        source: AccountAddress,
        destination: AccountAddress,
        currencies: Vec<CurrencyAddress>,
    },
    Destroy {
        currencies: Vec<CurrencyAddress>,
    },
    LeakRepair {
        leaked: Vec<CurrencyAddress>,
        leaked_owners: Vec<AccountAddress>,
        reserve: Vec<CurrencyAddress>,
        replacement_reserve: Vec<CurrencyAddress>,
    },
}

#[derive(Clone, Debug)]
struct PreparedTask {
    task: VerifiedLegalTask,
    operations: Vec<PreparedOperation>,
}

#[derive(Clone, Debug)]
enum BuildOutcome {
    Prepared(Box<PreparedTask>),
    AlreadySucceeded,
}

#[derive(Clone, Debug)]
pub struct PreparedTaskBook {
    tasks: BTreeMap<TaskId, PreparedTask>,
    claims: CurrencyClaimBook,
    store: StateStore,
}

impl PreparedTaskBook {
    pub fn new(store: StateStore) -> Self {
        Self {
            tasks: BTreeMap::new(),
            claims: CurrencyClaimBook::new(),
            store,
        }
    }

    pub fn prepare(
        &mut self,
        state: &mut SecondState,
        task: &VerifiedLegalTask,
        now: u64,
        validator_set: &ValidatorSet,
    ) -> Result<PreparationOutcome, PreparationError> {
        if self.tasks.contains_key(&task.task_id()) {
            return Err(PreparationError::AlreadyPrepared(task.task_id()));
        }

        let before_frontier = state.next_currency_address();
        let before_binding = state.bound_request_digest(task.task_id());
        let mut candidate = state.clone();

        let result = self.build_prepared(&mut candidate, task, now);
        let protocol_changed = candidate.next_currency_address() != before_frontier
            || candidate.bound_request_digest(task.task_id()) != before_binding;

        match result {
            Ok(BuildOutcome::AlreadySucceeded) => Ok(PreparationOutcome::AlreadySucceeded),
            Ok(BuildOutcome::Prepared(prepared)) => {
                if protocol_changed {
                    if let Err(error) = self.store.save(&candidate, validator_set) {
                        self.claims.release_task(task.task_id());
                        return Err(error.into());
                    }
                    *state = candidate;
                }

                self.tasks.insert(task.task_id(), *prepared);
                Ok(PreparationOutcome::Prepared)
            }
            Err(error) => {
                self.claims.release_task(task.task_id());

                if protocol_changed {
                    if let Err(persistence_error) = self.store.save(&candidate, validator_set) {
                        return Err(persistence_error.into());
                    }
                    *state = candidate;
                }

                Err(error)
            }
        }
    }

    pub fn commit(
        &mut self,
        state: &mut SecondState,
        task_id: TaskId,
        now: u64,
        validator_set: &ValidatorSet,
    ) -> Result<ExecutionOutcome, PreparationError> {
        let prepared = self
            .tasks
            .get(&task_id)
            .cloned()
            .ok_or(PreparationError::NotPrepared(task_id))?;

        if prepared
            .task
            .expires_at()
            .is_some_and(|expires_at| now > expires_at)
        {
            self.tasks.remove(&task_id);
            self.claims.release_task(task_id);
            return Err(ExecutionError::TaskExpired.into());
        }

        let mut candidate = state.clone();
        let outcome = match self.commit_inner(&mut candidate, &prepared) {
            Ok(outcome) => outcome,
            Err(error) => {
                self.tasks.remove(&task_id);
                self.claims.release_task(task_id);
                return Err(error);
            }
        };

        if outcome == ExecutionOutcome::Succeeded {
            self.store.save(&candidate, validator_set)?;
            *state = candidate;
        }

        self.tasks.remove(&task_id);
        self.claims.release_task(task_id);
        Ok(outcome)
    }

    pub fn cancel(&mut self, task_id: TaskId) -> Result<(), PreparationError> {
        if self.tasks.remove(&task_id).is_none() {
            return Err(PreparationError::NotPrepared(task_id));
        }

        self.claims.release_task(task_id);
        Ok(())
    }

    pub fn prepared_count(&self) -> usize {
        self.tasks.len()
    }

    pub fn claimed_currency_count(&self) -> usize {
        self.claims.claimed_currency_count()
    }

    pub fn is_prepared(&self, task_id: TaskId) -> bool {
        self.tasks.contains_key(&task_id)
    }

    fn build_prepared(
        &mut self,
        state: &mut SecondState,
        task: &VerifiedLegalTask,
        now: u64,
    ) -> Result<BuildOutcome, PreparationError> {
        if state.bind_task(task)? {
            return Ok(BuildOutcome::AlreadySucceeded);
        }

        if task.expires_at().is_some_and(|expires_at| now > expires_at) {
            return Err(ExecutionError::TaskExpired.into());
        }

        let mut working = state.business.clone();
        let operations = self.prepare_operations(state, task, &mut working)?;

        Ok(BuildOutcome::Prepared(Box::new(PreparedTask {
            task: task.clone(),
            operations,
        })))
    }

    fn prepare_operations(
        &mut self,
        state: &mut SecondState,
        task: &VerifiedLegalTask,
        working: &mut BusinessState,
    ) -> Result<Vec<PreparedOperation>, PreparationError> {
        let mut prepared = Vec::with_capacity(task.operations().len());
        let mut operation_index = 0_u64;

        for operation in task.operations() {
            let claim_id = OperationClaimId::new(task.task_id(), operation_index);

            let prepared_operation = match operation {
                Operation::Issue { account, count } => {
                    state.require_account(working, *account)?;
                    let addresses = state.allocate_currency_range(*count)?;
                    state.apply_issue_preallocated(working, *account, &addresses)?;

                    PreparedOperation::Issue {
                        account: *account,
                        addresses,
                    }
                }
                Operation::Transfer {
                    source,
                    destination,
                    amount,
                } => {
                    state.require_account(working, *source)?;
                    state.require_account(working, *destination)?;

                    let currencies = self
                        .claims
                        .claim_transfer_in_business_state(working, claim_id, *source, *amount)?;
                    state.claim_transfer_candidates(working, *source, *destination, &currencies)?;

                    PreparedOperation::Transfer {
                        source: *source,
                        destination: *destination,
                        currencies,
                    }
                }
                Operation::Destroy { currencies } => {
                    state.validate_destroy_targets(working, currencies)?;
                    self.claims.claim_explicit(claim_id, currencies)?;
                    state.apply_destroy(working, currencies);

                    PreparedOperation::Destroy {
                        currencies: currencies.clone(),
                    }
                }
                Operation::LeakRepair { leaked } => {
                    let leaked_owners = state.validate_leaked_owners(working, leaked)?;
                    let reserve = self
                        .claims
                        .claim_leak_repair_in_business_state(claim_id, working, leaked)?;
                    let replacement_count = u64::try_from(leaked.len())
                        .map_err(|_| PreparationError::LengthOverflow)?;
                    let replacement_reserve = state.allocate_currency_range(replacement_count)?;

                    state.apply_leak_repair_preallocated(
                        working,
                        leaked,
                        &leaked_owners,
                        &reserve,
                        &replacement_reserve,
                    )?;

                    PreparedOperation::LeakRepair {
                        leaked: leaked.clone(),
                        leaked_owners,
                        reserve,
                        replacement_reserve,
                    }
                }
            };

            prepared.push(prepared_operation);
            operation_index = operation_index
                .checked_add(1)
                .ok_or(PreparationError::OperationIndexOverflow)?;
        }

        Ok(prepared)
    }

    fn commit_inner(
        &self,
        state: &mut SecondState,
        prepared: &PreparedTask,
    ) -> Result<ExecutionOutcome, PreparationError> {
        if state.bind_task(&prepared.task)? {
            return Ok(ExecutionOutcome::AlreadySucceeded);
        }

        let mut working = state.business.clone();

        for operation in &prepared.operations {
            match operation {
                PreparedOperation::Issue { account, addresses } => {
                    state.apply_issue_preallocated(&mut working, *account, addresses)?;
                }
                PreparedOperation::Transfer {
                    source,
                    destination,
                    currencies,
                } => {
                    state.claim_transfer_candidates(
                        &mut working,
                        *source,
                        *destination,
                        currencies,
                    )?;
                }
                PreparedOperation::Destroy { currencies } => {
                    state.validate_destroy_targets(&working, currencies)?;
                    state.apply_destroy(&mut working, currencies);
                }
                PreparedOperation::LeakRepair {
                    leaked,
                    leaked_owners,
                    reserve,
                    replacement_reserve,
                } => {
                    state.apply_leak_repair_preallocated(
                        &mut working,
                        leaked,
                        leaked_owners,
                        reserve,
                        replacement_reserve,
                    )?;
                }
            }
        }

        state.business = working;
        state.mark_task_succeeded(prepared.task.task_id());
        Ok(ExecutionOutcome::Succeeded)
    }
}
