use std::collections::BTreeMap;

use ed25519_dalek::SigningKey;

use crate::prepared_plan::{PreparedOperation, PreparedTask};
use crate::state::BusinessState;
use crate::{
    CURRENT_PROTOCOL_VERSION, ClaimError, CurrencyClaimBook, ExecutionError, ExecutionOutcome,
    FinalityCertificate, FinalityError, FinalityStatement, Operation, OperationClaimId,
    PersistenceError, SecondState, StateStore, TaskId, ValidatorId, ValidatorSet, ValidatorSigner,
    ValidatorSigningError, ValidatorVote, VerifiedLegalTask,
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
    store: StateStore,
}

impl PreparedTaskBook {
    pub fn new(store: StateStore) -> Result<Self, PreparationError> {
        let tasks = store.load_prepared_tasks()?;
        let mut claims = CurrencyClaimBook::new();

        for prepared in tasks.values() {
            prepared.restore_claims(&mut claims)?;
        }

        Ok(Self {
            tasks,
            claims,
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
        let before_binding = state.bound_legality_proof(task.task_id());
        let mut candidate = state.clone();

        let result = self.build_prepared(&mut candidate, task, now, validator_set.version());
        let durable_prerequisite_changed = candidate.prerequisite != state.prerequisite;
        let protocol_changed = candidate.next_currency_address() != before_frontier
            || candidate.bound_legality_proof(task.task_id()) != before_binding;

        match result {
            Ok(BuildOutcome::AlreadySucceeded) => Ok(PreparationOutcome::AlreadySucceeded),
            Ok(BuildOutcome::Prepared(prepared)) => {
                self.tasks.insert(task.task_id(), prepared);

                if let Err(error) =
                    self.store
                        .save_with_prepared(&candidate, validator_set, &self.tasks)
                {
                    self.tasks.remove(&task.task_id());
                    self.claims.release_task(task.task_id());
                    return Err(error.into());
                }

                *state = candidate;
                Ok(PreparationOutcome::Prepared)
            }
            Err(error) => {
                self.claims.release_task(task.task_id());

                if protocol_changed || durable_prerequisite_changed {
                    self.store
                        .save_with_prepared(&candidate, validator_set, &self.tasks)?;
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
        validator_set: &ValidatorSet,
    ) -> Result<ExecutionOutcome, PreparationError> {
        let prepared = self
            .tasks
            .get(&task_id)
            .cloned()
            .ok_or(PreparationError::NotPrepared(task_id.clone()))?;

        let expected_statement =
            self.prepared_finality_statement(task_id.clone(), validator_set)?;
        let actual_statement = certificate.statement();

        if actual_statement.subject_digest() != expected_statement.subject_digest() {
            return Err(PreparationError::FinalitySubjectMismatch {
                expected: expected_statement.subject_digest(),
                actual: actual_statement.subject_digest(),
            });
        }

        certificate.verify(validator_set)?;

        let mut candidate = state.clone();
        let outcome = match self.commit_inner(&mut candidate, &prepared) {
            Ok(outcome) => outcome,
            Err(error) => {
                self.remove_prepared_durably(task_id)?;
                return Err(error);
            }
        };

        let mut remaining = self.tasks.clone();
        remaining.remove(&task_id);

        match outcome {
            ExecutionOutcome::Succeeded => {
                self.store
                    .save_with_prepared(&candidate, validator_set, &remaining)?;
                *state = candidate;
            }
            ExecutionOutcome::AlreadySucceeded => {
                self.store.replace_prepared_tasks(&remaining)?;
            }
        }

        self.tasks = remaining;
        self.claims.release_task(task_id);
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
        validator_set: &ValidatorSet,
    ) -> Result<FinalityStatement, PreparationError> {
        let prepared = self
            .tasks
            .get(&task_id)
            .ok_or(PreparationError::NotPrepared(task_id.clone()))?;

        if prepared.validator_set_version != validator_set.version() {
            return Err(PreparationError::ValidatorSetVersionChanged {
                expected: prepared.validator_set_version,
                actual: validator_set.version(),
            });
        }

        Ok(FinalityStatement::new(
            CURRENT_PROTOCOL_VERSION,
            prepared.validator_set_version,
            prepared.plan_digest()?,
        ))
    }

    pub fn sign_prepared_vote(
        &self,
        task_id: TaskId,
        validator_id: ValidatorId,
        signing_key: &SigningKey,
        validator_set: &ValidatorSet,
    ) -> Result<ValidatorVote, PreparationError> {
        let statement = self.prepared_finality_statement(task_id.clone(), validator_set)?;
        let signer = ValidatorSigner::new(validator_id, signing_key.clone(), self.store.clone());
        signer
            .sign_prepared_task(task_id, &statement, validator_set)
            .map_err(PreparationError::from)
    }

    pub fn cancel(&mut self, task_id: TaskId) -> Result<(), PreparationError> {
        if !self.tasks.contains_key(&task_id) {
            return Err(PreparationError::NotPrepared(task_id));
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

    fn remove_prepared_durably(&mut self, task_id: TaskId) -> Result<(), PreparationError> {
        let mut remaining = self.tasks.clone();
        if remaining.remove(&task_id).is_none() {
            return Err(PreparationError::NotPrepared(task_id));
        }

        self.store.replace_prepared_tasks(&remaining)?;
        self.tasks = remaining;
        self.claims.release_task(task_id);
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

        if now > task.expires_at() {
            return Err(ExecutionError::TaskExpired.into());
        }

        let mut working = state.business.clone();
        let operations = self.prepare_operations(state, task, &mut working)?;

        Ok(BuildOutcome::Prepared(PreparedTask::new(
            task.task_id(),
            task.legality_proof(),
            task.expires_at(),
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
                    let expires_at = task.expires_at();
                    let transfer = state.establish_transfer(
                        claim_id.clone(),
                        *source,
                        *destination,
                        *amount,
                        expires_at,
                    )?;

                    state.validate_established_transfer_for_execution(working, transfer)?;

                    let currencies = self.claims.claim_transfer_in_business_state(
                        working,
                        claim_id,
                        transfer.source_account,
                        *amount,
                    )?;
                    state.claim_transfer_candidates(
                        working,
                        transfer.source_account,
                        transfer.destination_account,
                        &currencies,
                    )?;

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
            };

            prepared.push(prepared_operation);
        }

        Ok(prepared)
    }

    fn commit_inner(
        &self,
        state: &mut SecondState,
        prepared: &PreparedTask,
    ) -> Result<ExecutionOutcome, PreparationError> {
        if state.bind_task_legality_proof(prepared.task_id.clone(), prepared.legality_proof)? {
            return Ok(ExecutionOutcome::AlreadySucceeded);
        }

        let mut working = state.business.clone();
        prepared.apply(state, &mut working)?;

        state.business = working;
        state.mark_task_succeeded(prepared.task_id.clone());
        Ok(ExecutionOutcome::Succeeded)
    }
}
