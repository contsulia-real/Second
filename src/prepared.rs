use std::collections::BTreeMap;

use ed25519_dalek::SigningKey;

use crate::prepared_plan::{PreparedOperation, PreparedTask, PreparedTaskPhase};
use crate::state::BusinessState;
use crate::{
    CURRENT_PROTOCOL_VERSION, ClaimError, CurrencyClaimBook, ExecutionError, ExecutionOutcome,
    FinalityCertificate, FinalityError, FinalityStatement, Operation, OperationClaimId,
    PaymentAddress, PersistenceError, SecondState, StateStore, TaskId, ValidatorId, ValidatorSet,
    ValidatorSigner, ValidatorSigningError, ValidatorVote, VerifiedLegalTask,
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
    Finality(FinalityError),
    Signing(ValidatorSigningError),
    FinalitySubjectMismatch {
        expected: [u8; 32],
        actual: [u8; 32],
    },
    ValidatorSetVersionChanged {
        expected: u64,
        actual: u64,
    },
    AlreadyPrepared(TaskId),
    NotPrepared(TaskId),
    CancellationClosed(TaskId),
    PaymentAddressContention(PaymentAddress),
    InvalidPreparedPlan,
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

impl From<FinalityError> for PreparationError {
    fn from(error: FinalityError) -> Self {
        Self::Finality(error)
    }
}

impl From<ValidatorSigningError> for PreparationError {
    fn from(error: ValidatorSigningError) -> Self {
        Self::Signing(error)
    }
}

#[derive(Clone, Debug)]
enum BuildOutcome {
    Prepared(PreparedTask),
    AlreadySucceeded,
}

#[derive(Clone, Debug)]
pub struct PreparedTaskBook {
    tasks: BTreeMap<TaskId, PreparedTask>,
    claims: CurrencyClaimBook,
    payment_address_claims: BTreeMap<PaymentAddress, TaskId>,
    store: StateStore,
}

impl PreparedTaskBook {
    pub fn new(store: StateStore) -> Result<Self, PreparationError> {
        let tasks = store.load_prepared_tasks()?;
        let mut claims = CurrencyClaimBook::new();
        let mut payment_address_claims = BTreeMap::new();

        for prepared in tasks.values() {
            prepared.restore_claims(&mut claims)?;
            prepared.restore_payment_address_claims(&mut payment_address_claims)?;
        }

        Ok(Self {
            tasks,
            claims,
            payment_address_claims,
            store,
        })
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

        let result = self.build_prepared(&mut candidate, task, now, validator_set.version());
        let durable_prerequisite_changed = candidate.prerequisite != state.prerequisite;
        let protocol_changed = candidate.next_currency_address() != before_frontier
            || candidate.bound_request_digest(task.task_id()) != before_binding;

        match result {
            Ok(BuildOutcome::AlreadySucceeded) => Ok(PreparationOutcome::AlreadySucceeded),
            Ok(BuildOutcome::Prepared(prepared)) => {
                let mut updated_tasks = self.tasks.clone();
                updated_tasks.insert(task.task_id(), prepared);

                if let Err(error) = self.store.save_with_prepared(
                    state,
                    &candidate,
                    validator_set,
                    &self.tasks,
                    &updated_tasks,
                ) {
                    self.release_task_claims(task.task_id());
                    return Err(error.into());
                }

                self.tasks = updated_tasks;
                *state = candidate;
                Ok(PreparationOutcome::Prepared)
            }
            Err(error) => {
                self.release_task_claims(task.task_id());

                if protocol_changed || durable_prerequisite_changed {
                    self.store.save_with_prepared(
                        state,
                        &candidate,
                        validator_set,
                        &self.tasks,
                        &self.tasks,
                    )?;
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
        certificate: &FinalityCertificate,
    ) -> Result<ExecutionOutcome, PreparationError> {
        let prepared = self
            .tasks
            .get(&task_id)
            .cloned()
            .ok_or(PreparationError::NotPrepared(task_id.clone()))?;

        let (expected_statement, validator_set) =
            self.prepared_finality_context(task_id.clone())?;
        let actual_statement = certificate.statement();

        if actual_statement.subject_digest() != expected_statement.subject_digest() {
            return Err(PreparationError::FinalitySubjectMismatch {
                expected: expected_statement.subject_digest(),
                actual: actual_statement.subject_digest(),
            });
        }

        certificate.verify(&validator_set)?;
        self.advance_phase_durably(task_id.clone(), PreparedTaskPhase::Finalized)?;

        let mut candidate = state.clone();
        let outcome = self.commit_inner(&mut candidate, &prepared)?;

        let mut remaining = self.tasks.clone();
        remaining.remove(&task_id);

        match outcome {
            ExecutionOutcome::Succeeded => {
                self.store
                    .commit_prepared_state(state, &candidate, &self.tasks, &remaining)?;
                *state = candidate;
            }
            ExecutionOutcome::AlreadySucceeded => {
                self.store.replace_prepared_tasks(&self.tasks, &remaining)?;
            }
        }

        self.tasks = remaining;
        self.release_task_claims(task_id);
        Ok(outcome)
    }

    pub fn prepared_plan_digest(&self, task_id: TaskId) -> Result<[u8; 32], PreparationError> {
        self.tasks
            .get(&task_id)
            .ok_or(PreparationError::NotPrepared(task_id))?
            .plan_digest()
    }

    pub fn prepared_finality_statement(
        &self,
        task_id: TaskId,
    ) -> Result<FinalityStatement, PreparationError> {
        self.prepared_finality_context(task_id)
            .map(|(statement, _)| statement)
    }

    fn prepared_finality_context(
        &self,
        task_id: TaskId,
    ) -> Result<(FinalityStatement, ValidatorSet), PreparationError> {
        let prepared = self
            .tasks
            .get(&task_id)
            .ok_or(PreparationError::NotPrepared(task_id.clone()))?;
        let plan_digest = prepared.plan_digest()?;
        let validator_set = self
            .store
            .validator_set_for_prepared_task(&task_id, plan_digest)?;

        Ok((
            FinalityStatement::new(
                CURRENT_PROTOCOL_VERSION,
                prepared.validator_set_version,
                plan_digest,
            ),
            validator_set,
        ))
    }

    pub fn sign_prepared_vote(
        &mut self,
        task_id: TaskId,
        validator_id: ValidatorId,
        signing_key: &SigningKey,
    ) -> Result<ValidatorVote, PreparationError> {
        let (statement, validator_set) = self.prepared_finality_context(task_id.clone())?;
        let signer = ValidatorSigner::new(validator_id, signing_key.clone(), self.store.clone());
        let vote = signer
            .sign_prepared_task(task_id.clone(), &statement, &validator_set)
            .map_err(PreparationError::from)?;

        self.tasks
            .get_mut(&task_id)
            .ok_or(PreparationError::NotPrepared(task_id))?
            .advance_phase(PreparedTaskPhase::Voting);
        Ok(vote)
    }

    pub fn cancel(&mut self, task_id: TaskId) -> Result<(), PreparationError> {
        let prepared = self
            .tasks
            .get(&task_id)
            .ok_or(PreparationError::NotPrepared(task_id.clone()))?;

        if prepared.phase != PreparedTaskPhase::Prepared {
            return Err(PreparationError::CancellationClosed(task_id));
        }

        self.remove_prepared_durably(task_id)
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

    fn advance_phase_durably(
        &mut self,
        task_id: TaskId,
        phase: PreparedTaskPhase,
    ) -> Result<(), PreparationError> {
        let current = self
            .tasks
            .get(&task_id)
            .ok_or(PreparationError::NotPrepared(task_id.clone()))?;

        if phase <= current.phase {
            return Ok(());
        }

        let plan_digest = current.plan_digest()?;
        self.store
            .advance_prepared_task_phase(&task_id, plan_digest, phase)?;
        self.tasks
            .get_mut(&task_id)
            .ok_or(PreparationError::NotPrepared(task_id))?
            .advance_phase(phase);
        Ok(())
    }

    fn remove_prepared_durably(&mut self, task_id: TaskId) -> Result<(), PreparationError> {
        let mut remaining = self.tasks.clone();
        if remaining.remove(&task_id).is_none() {
            return Err(PreparationError::NotPrepared(task_id));
        }

        self.store.replace_prepared_tasks(&self.tasks, &remaining)?;
        self.tasks = remaining;
        self.release_task_claims(task_id);
        Ok(())
    }

    fn build_prepared(
        &mut self,
        state: &mut SecondState,
        task: &VerifiedLegalTask,
        now: u64,
        validator_set_version: u64,
    ) -> Result<BuildOutcome, PreparationError> {
        if state.bind_task(task)? {
            return Ok(BuildOutcome::AlreadySucceeded);
        }

        if task.is_expired(now) {
            return Err(ExecutionError::TaskExpired.into());
        }

        let mut working = state.business.clone();
        let operations = self.prepare_operations(state, task, &mut working)?;

        Ok(BuildOutcome::Prepared(PreparedTask::new(
            task.task_id(),
            task.request_digest(),
            validator_set_version,
            operations,
        )))
    }

    fn prepare_operations(
        &mut self,
        state: &mut SecondState,
        task: &VerifiedLegalTask,
        working: &mut BusinessState,
    ) -> Result<Vec<PreparedOperation>, PreparationError> {
        let mut prepared = Vec::with_capacity(task.operations().len());
        let mut simulated_prerequisite = state.prerequisite.clone();

        for (index, operation) in task.operations().iter().enumerate() {
            let operation_index =
                u64::try_from(index).map_err(|_| PreparationError::OperationIndexOverflow)?;
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
                    let transfer = state.establish_transfer(
                        working,
                        claim_id.clone(),
                        *source,
                        *destination,
                        *amount,
                    )?;

                    state.validate_established_transfer_for_execution(working, transfer)?;

                    let currencies = self.claims.claim_transfer_in_business_state(
                        working,
                        claim_id.clone(),
                        transfer.source_account,
                        *amount,
                    )?;
                    state.claim_transfer_candidates(
                        working,
                        transfer.source_account,
                        transfer.destination_account,
                        &currencies,
                    )?;
                    simulated_prerequisite.payment_executions.remove(&claim_id);

                    PreparedOperation::Transfer {
                        transfer,
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
                Operation::RegisterPaymentAddress { address, account } => {
                    self.claim_payment_address(task.task_id(), *address)?;
                    state
                        .register_payment_address_in_business_state(working, *address, *account)?;
                    PreparedOperation::RegisterPaymentAddress {
                        address: *address,
                        account: *account,
                    }
                }
                Operation::RetirePaymentAddress { address } => {
                    self.claim_payment_address(task.task_id(), *address)?;
                    state.retire_payment_address_in_business_state(working, *address)?;
                    PreparedOperation::RetirePaymentAddress { address: *address }
                }
                Operation::FinalizePaymentAddressRetirement { address } => {
                    self.claim_payment_address(task.task_id(), *address)?;
                    state.finalize_payment_address_retirement_in_business_state(
                        working,
                        &simulated_prerequisite,
                        *address,
                    )?;
                    PreparedOperation::FinalizePaymentAddressRetirement { address: *address }
                }
            };

            prepared.push(prepared_operation);
        }

        Ok(prepared)
    }

    fn claim_payment_address(
        &mut self,
        task_id: TaskId,
        address: PaymentAddress,
    ) -> Result<(), PreparationError> {
        match self.payment_address_claims.get(&address) {
            Some(owner) if owner != &task_id => {
                Err(PreparationError::PaymentAddressContention(address))
            }
            Some(_) => Ok(()),
            None => {
                self.payment_address_claims.insert(address, task_id);
                Ok(())
            }
        }
    }

    fn release_task_claims(&mut self, task_id: TaskId) {
        self.claims.release_task(task_id.clone());
        self.payment_address_claims
            .retain(|_, owner| owner != &task_id);
    }

    fn commit_inner(
        &self,
        state: &mut SecondState,
        prepared: &PreparedTask,
    ) -> Result<ExecutionOutcome, PreparationError> {
        if state.bind_task_request_digest(prepared.task_id.clone(), prepared.request_digest)? {
            return Ok(ExecutionOutcome::AlreadySucceeded);
        }

        let mut working = state.business.clone();
        prepared.apply(state, &mut working)?;

        state.business = working;
        state.mark_task_succeeded(prepared.task_id.clone());
        Ok(ExecutionOutcome::Succeeded)
    }
}
