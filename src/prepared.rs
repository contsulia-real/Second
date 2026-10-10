#[cfg(test)]
mod abort_tests;
mod certified;
mod conflict;
mod handoff_admission;
mod variants;
pub(crate) use conflict::VerifiedContention;
#[cfg(test)]
mod conflict_tests;
pub(crate) mod fences;
#[cfg(test)]
mod handoff_tests;
pub(crate) mod lifecycle;
#[cfg(test)]
mod repair_pairing_tests;
pub(crate) mod source;
#[cfg(test)]
mod source_tests;
#[cfg(test)]
pub(crate) mod tests;

use std::collections::BTreeMap;

use ed25519_dalek::SigningKey;

use crate::prepared_plan::{PreparedOperation, PreparedTask, PreparedTaskPhase};
use crate::state::BusinessState;
use crate::{
    AccountAddress, CURRENT_PROTOCOL_VERSION, ClaimError, CurrencyClaimBook, ExecutionError,
    ExecutionOutcome, FinalityCertificate, FinalityError, FinalityStatement, Operation,
    OperationClaimId, PaymentAddress, PersistenceError, SecondState, StateStore, TaskId,
    ValidatorId, ValidatorSet, ValidatorSigner, ValidatorSigningError, ValidatorVote,
    VerifiedLegalTask,
};
use lifecycle::{LifecycleAddress, LifecycleClaimBook};

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
    PreparedPlanDigestMismatch {
        expected: [u8; 32],
        actual: [u8; 32],
    },
    AlreadyPrepared(TaskId),
    NotPrepared(TaskId),
    CertifiedDependencyPending(TaskId),
    CancellationClosed(TaskId),
    PaymentAddressContention(PaymentAddress),
    AccountContention(AccountAddress),
    VerifiedResourceContention {
        task_id: TaskId,
        blockers: Vec<TaskId>,
    },
    CertifiedResourceFence {
        blockers: Vec<TaskId>,
    },
    InvalidPreparedPlan,
    TaskOriginMismatch {
        expected: u64,
        actual: u64,
    },
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
    Prepared(Box<PreparedTask>),
    AlreadySucceeded,
}

#[derive(Clone, Debug)]
pub struct PreparedTaskBook {
    tasks: BTreeMap<TaskId, PreparedTask>,
    claims: CurrencyClaimBook,
    lifecycle_claims: LifecycleClaimBook,
    store: StateStore,
}

impl PreparedTaskBook {
    pub fn new(store: StateStore) -> Result<Self, PreparationError> {
        let tasks = store.load_prepared_tasks()?;
        Self::from_tasks(store, tasks)
    }

    pub(crate) fn from_tasks(
        store: StateStore,
        tasks: BTreeMap<TaskId, PreparedTask>,
    ) -> Result<Self, PreparationError> {
        let mut claims = CurrencyClaimBook::new();
        let mut lifecycle_claims = LifecycleClaimBook::default();

        for prepared in tasks.values() {
            if !prepared.has_owned_candidate() {
                continue;
            }
            prepared.restore_owned_claims(&mut claims)?;
            prepared.restore_owned_lifecycle_claims(&mut lifecycle_claims)?;
        }

        Ok(Self {
            tasks,
            claims,
            lifecycle_claims,
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
        self.prepare_inner(state, task, now, validator_set, None, None)
    }

    pub(crate) fn prepare_expected_plan(
        &mut self,
        state: &mut SecondState,
        task: &VerifiedLegalTask,
        now: u64,
        validator_set: &ValidatorSet,
        expected_plan_digest: [u8; 32],
        selections: &[crate::AddressRanges],
    ) -> Result<PreparationOutcome, PreparationError> {
        self.prepare_inner(
            state,
            task,
            now,
            validator_set,
            Some(expected_plan_digest),
            Some(selections),
        )
    }

    fn prepare_inner(
        &mut self,
        state: &mut SecondState,
        task: &VerifiedLegalTask,
        now: u64,
        validator_set: &ValidatorSet,
        expected_plan_digest: Option<[u8; 32]>,
        selections: Option<&[crate::AddressRanges]>,
    ) -> Result<PreparationOutcome, PreparationError> {
        if let Some(existing) = self.tasks.get(&task.task_id())
            && !existing.commit_authorized
            && (existing.conflict_abort || expected_plan_digest != Some(existing.plan_digest()?))
        {
            return Err(PreparationError::AlreadyPrepared(task.task_id()));
        }
        if self.tasks.get(&task.task_id()).is_some_and(|plan| {
            plan.has_owned_candidate() || plan.phase == PreparedTaskPhase::Finalized
        }) {
            return Err(PreparationError::AlreadyPrepared(task.task_id()));
        }

        if let Some(handoff) = &state.protocol.task_handoff {
            handoff.validate_task_origin(state, &task.task_id(), validator_set.version())?;
        }

        let before_frontier = state.next_currency_address();
        let before_binding = state.bound_request_digest(task.task_id());
        let mut candidate = state.clone();

        // This exact source was fully admitted before expiry. Retrying its
        // resource acquisition does not admit a new request after expiry.
        let now = self.admitted_request_time(
            state,
            task,
            validator_set.version(),
            expected_plan_digest,
            now,
        );

        let result = self.build_prepared(
            &mut candidate,
            task,
            now,
            validator_set.version(),
            selections,
        );
        let durable_prerequisite_changed = candidate.prerequisite != state.prerequisite;
        let protocol_changed = candidate.next_currency_address() != before_frontier
            || candidate.bound_request_digest(task.task_id()) != before_binding;

        match result {
            Ok(BuildOutcome::AlreadySucceeded) => Ok(PreparationOutcome::AlreadySucceeded),
            Ok(BuildOutcome::Prepared(prepared)) => {
                let mut prepared = prepared;
                if let Some(existing) = self.tasks.get(&task.task_id())
                    && !existing.commit_authorized
                {
                    prepared.advance_phase(existing.phase);
                    prepared.variants = existing.variants.clone();
                }
                if let Err(error) = prepared.encode_source() {
                    self.release_task_claims(task.task_id());
                    return Err(error);
                }
                if let Some(expected) = expected_plan_digest {
                    let actual = match prepared.plan_digest() {
                        Ok(actual) => actual,
                        Err(error) => {
                            self.release_task_claims(task.task_id());
                            return Err(error);
                        }
                    };
                    if actual != expected {
                        self.release_task_claims(task.task_id());
                        return Err(PreparationError::PreparedPlanDigestMismatch {
                            expected,
                            actual,
                        });
                    }
                }
                if let Some(handoff) = &state.protocol.task_handoff {
                    let blockers = match handoff.blockers(state, &prepared) {
                        Ok(blockers) => blockers,
                        Err(error) => {
                            self.release_task_claims(task.task_id());
                            return Err(error);
                        }
                    };
                    if !blockers.is_empty() {
                        self.release_task_claims(task.task_id());
                        return Err(PreparationError::CertifiedResourceFence { blockers });
                    }
                }
                let persistence_set = match self.persistence_validator_set(
                    state,
                    &task.task_id(),
                    validator_set,
                    expected_plan_digest,
                ) {
                    Ok(set) => set,
                    Err(error) => {
                        self.release_task_claims(task.task_id());
                        return Err(error);
                    }
                };
                let mut updated_tasks = self.tasks.clone();
                updated_tasks.insert(task.task_id(), *prepared);
                if let Some(binding) = candidate.protocol.task_bindings.get_mut(&task.task_id()) {
                    binding.allocation_task = None;
                }

                if let Err(error) = self.store.save_with_prepared(
                    state,
                    &candidate,
                    &persistence_set,
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

                // Unauthorized composite tasks must never reserve their TaskId
                // or create durable execution prerequisites before rejection.
                if matches!(
                    &error,
                    PreparationError::Execution(ExecutionError::MissingAccountSignature(_))
                ) {
                    return Err(error);
                }

                // A peer's frozen choices are untrusted until the full plan digest matches.
                // Rejected sources cannot create durable payment prerequisites or bindings.
                if expected_plan_digest.is_some() {
                    return Err(error);
                }

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

    /// Release only after the store atomically installs an exact-committee Abort proof.
    pub fn abort_certified(
        &mut self,
        state: &mut SecondState,
        task_id: TaskId,
        certificate: &FinalityCertificate,
    ) -> Result<(), PreparationError> {
        self.store.install_prepared_abort(&task_id, certificate)?;
        let snapshot = self
            .store
            .load()?
            .ok_or(PersistenceError::MissingSnapshot)?;
        *state = snapshot.state;
        *self = Self::new(self.store.clone())?;
        Ok(())
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
            .ok_or(PreparationError::NotPrepared(task_id.clone()))?
            .candidate(certificate.statement().subject_digest())?
            .filter(|plan| plan.commit_authorized)
            .ok_or(PreparationError::NotPrepared(task_id.clone()))?;

        let validator_set = self
            .store
            .validator_set_for_prepared_task(&task_id, prepared.plan_digest()?)?;
        let expected_statement = FinalityStatement::new(
            CURRENT_PROTOCOL_VERSION,
            validator_set.version(),
            prepared.plan_digest()?,
        );
        let actual_statement = certificate.statement();

        if actual_statement.subject_digest() != expected_statement.subject_digest() {
            return Err(PreparationError::FinalitySubjectMismatch {
                expected: expected_statement.subject_digest(),
                actual: actual_statement.subject_digest(),
            });
        }

        certificate.verify(&validator_set)?;
        let plan_digest = prepared.plan_digest()?;
        self.store
            .finalize_prepared_task(&task_id, plan_digest, certificate)?;
        // Keep the caller's book closed to cancellation even if execution fails.
        self.tasks = self.store.load_prepared_tasks()?;
        let outcome = Self::commit_certified_component(&self.store, &task_id, Some(state))?;
        if !outcome.completed.contains(&task_id) {
            return Err(PreparationError::CertifiedDependencyPending(task_id));
        }
        *state = self
            .store
            .load()?
            .ok_or(PersistenceError::MissingSnapshot)?
            .state;
        *self = Self::new(self.store.clone())?;
        Ok(ExecutionOutcome::Succeeded)
    }

    pub(crate) fn recover_finalized_from_store(store: &StateStore) -> Result<(), PreparationError> {
        let durable = store.load_prepared_tasks()?;
        for (task_id, prepared) in durable {
            if prepared.finality_votes.is_some() {
                Self::commit_certified_component(store, &task_id, None)?;
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn commit_certified_from_store(
        store: &StateStore,
        task_id: TaskId,
        certificate: &FinalityCertificate,
    ) -> Result<ExecutionOutcome, PreparationError> {
        const MAX_STALE_RETRIES: usize = 3;

        for attempt in 0..=MAX_STALE_RETRIES {
            let persisted = store.load()?.ok_or(PersistenceError::MissingSnapshot)?;
            let mut state = persisted.state;
            let mut book = Self::new(store.clone())?;
            match book.commit(&mut state, task_id.clone(), certificate) {
                Err(PreparationError::Persistence(
                    PersistenceError::StaleState | PersistenceError::StalePreparedTasks,
                )) if attempt < MAX_STALE_RETRIES => continue,
                result => return result,
            }
        }

        unreachable!("bounded commit retry loop always returns")
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

        if !prepared.commit_authorized || prepared.phase != PreparedTaskPhase::Prepared {
            return Err(PreparationError::CancellationClosed(task_id));
        }

        self.remove_prepared_durably(task_id)
    }

    pub fn prepared_count(&self) -> usize {
        self.tasks
            .values()
            .filter(|plan| plan.commit_authorized)
            .count()
    }

    pub fn claimed_currency_count(&self) -> u64 {
        self.claims.claimed_currency_count()
    }

    pub fn is_prepared(&self, task_id: TaskId) -> bool {
        self.tasks
            .get(&task_id)
            .is_some_and(|plan| plan.commit_authorized)
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
        selections: Option<&[crate::AddressRanges]>,
    ) -> Result<BuildOutcome, PreparationError> {
        state.authorize_task(task)?;
        if state.bind_task(task)? {
            return Ok(BuildOutcome::AlreadySucceeded);
        }

        if task.is_expired(now)
            && state
                .protocol
                .task_bindings
                .get(&task.task_id())
                .is_none_or(|binding| binding.allocation.is_none())
        {
            return Err(ExecutionError::TaskExpired.into());
        }

        let mut working = state.business.clone();
        let operations = self.prepare_operations(state, task, &mut working, selections)?;

        Ok(BuildOutcome::Prepared(Box::new(PreparedTask::new(
            task.task_id(),
            task.request_digest(),
            task.signed_task().clone(),
            validator_set_version,
            operations,
        ))))
    }

    fn prepare_operations(
        &mut self,
        state: &mut SecondState,
        task: &VerifiedLegalTask,
        working: &mut BusinessState,
        selections: Option<&[crate::AddressRanges]>,
    ) -> Result<Vec<PreparedOperation>, PreparationError> {
        let mut prepared = Vec::with_capacity(task.operations().len());
        let mut simulated_prerequisite = state.prerequisite.clone();
        let mut allocation_offset = 0;
        let mut selection_index = 0;

        for (index, operation) in task.operations().iter().enumerate() {
            let operation_index =
                u64::try_from(index).map_err(|_| PreparationError::OperationIndexOverflow)?;
            let claim_id = OperationClaimId::new(task.task_id(), operation_index);

            let prepared_operation = match operation {
                Operation::RegisterAccount { account } => {
                    task.require_account_signature(*account)?;
                    self.lifecycle_claims
                        .claim(task.task_id(), LifecycleAddress::Account(*account))?;
                    state.register_account_in_business_state(working, *account)?;
                    PreparedOperation::RegisterAccount { account: *account }
                }
                Operation::Issue { account, count } => {
                    state.require_account(working, *account)?;
                    let addresses =
                        state.allocated_task_addresses(task, allocation_offset, *count)?;
                    allocation_offset += *count;
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
                    if let Some(record) = working.payment_addresses.get(source) {
                        task.require_account_signature(record.account)?;
                    }
                    let transfer = state.establish_transfer(
                        working,
                        claim_id.clone(),
                        *source,
                        *destination,
                        *amount,
                    )?;

                    state.validate_established_transfer_for_execution(working, transfer)?;
                    task.require_account_signature(transfer.source_account)?;

                    let currencies = if let Some(selections) = selections {
                        let currencies = selections
                            .get(selection_index)
                            .ok_or(PreparationError::InvalidPreparedPlan)?;
                        selection_index += 1;
                        if currencies.len() != *amount {
                            return Err(PreparationError::InvalidPreparedPlan);
                        }
                        self.claims.restore_transfer(
                            claim_id.clone(),
                            transfer.source_account,
                            currencies,
                        )?;
                        currencies.clone()
                    } else {
                        self.claims.claim_transfer_in_business_state(
                            working,
                            claim_id.clone(),
                            transfer.source_account,
                            *amount,
                        )?
                    };
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
                    let seed = crate::reserve_sampling::ReserveSamplingSeed::new(
                        task.request_digest(),
                        operation_index,
                    );
                    let leaked_owners = state.validate_leaked_owners(working, leaked)?;
                    for owner in &leaked_owners {
                        task.require_account_signature(*owner)?;
                    }
                    let reserve = if let Some(selections) = selections {
                        let reserve = selections
                            .get(selection_index)
                            .ok_or(PreparationError::InvalidPreparedPlan)?;
                        selection_index += 1;
                        if reserve.len() != leaked.len() as u64 {
                            return Err(PreparationError::InvalidPreparedPlan);
                        }
                        self.claims.restore_leak_repair(claim_id, leaked, reserve)?;
                        reserve.clone()
                    } else {
                        self.claims
                            .claim_leak_repair_in_business_state(claim_id, working, leaked, &seed)?
                    };
                    let replacement_count = u64::try_from(leaked.len())
                        .map_err(|_| PreparationError::LengthOverflow)?;
                    let replacement_reserve = state.allocated_task_addresses(
                        task,
                        allocation_offset,
                        replacement_count,
                    )?;
                    allocation_offset += replacement_count;

                    state.apply_leak_repair_preallocated(
                        working,
                        leaked,
                        &leaked_owners,
                        &seed.pair(&reserve),
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
                    task.require_account_signature(*account)?;
                    self.lifecycle_claims
                        .claim(task.task_id(), LifecycleAddress::Payment(*address))?;
                    state
                        .register_payment_address_in_business_state(working, *address, *account)?;
                    PreparedOperation::RegisterPaymentAddress {
                        address: *address,
                        account: *account,
                    }
                }
                Operation::RetirePaymentAddress { address } => {
                    if let Some(record) = working.payment_addresses.get(address) {
                        task.require_account_signature(record.account)?;
                    }
                    self.lifecycle_claims
                        .claim(task.task_id(), LifecycleAddress::Payment(*address))?;
                    state.retire_payment_address_in_business_state(working, *address)?;
                    PreparedOperation::RetirePaymentAddress { address: *address }
                }
                Operation::FinalizePaymentAddressRetirement { address } => {
                    if let Some(record) = working.payment_addresses.get(address) {
                        task.require_account_signature(record.account)?;
                    }
                    self.lifecycle_claims
                        .claim(task.task_id(), LifecycleAddress::Payment(*address))?;
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

        if selections.is_some_and(|selections| selections.len() != selection_index) {
            return Err(PreparationError::InvalidPreparedPlan);
        }
        Ok(prepared)
    }

    fn release_task_claims(&mut self, task_id: TaskId) {
        self.claims.release_task(task_id.clone());
        self.lifecycle_claims.release_task(&task_id);
    }

    fn commit_inner(
        state: &mut SecondState,
        prepared: &PreparedTask,
    ) -> Result<ExecutionOutcome, PreparationError> {
        if state.bind_task_request_digest(prepared.task_id.clone(), prepared.request_digest)? {
            return Ok(ExecutionOutcome::AlreadySucceeded);
        }

        let mut working = state.business.clone();
        prepared.apply_certified(state, &mut working)?;

        state.business = working;
        state.mark_task_succeeded(prepared.task_id.clone());
        Ok(ExecutionOutcome::Succeeded)
    }
}
