//! Authorize and validate remote handoff bodies without importing resource rights.
use super::*;

#[cfg(test)]
mod tests;

impl PreparedTaskBook {
    /// The installed quorum-authenticated body supplies historical validation,
    /// never local resource ownership or payment prerequisites.
    pub(crate) fn admit_inherited_witness(
        &mut self,
        state: &SecondState,
        task_id: &TaskId,
        digest: [u8; 32],
    ) -> Result<(), PreparationError> {
        let snapshot = self
            .store
            .load_shared()?
            .ok_or(PersistenceError::MissingSnapshot)?;
        if !snapshot.state.same_persisted_state(state) || snapshot.prepared_tasks != self.tasks {
            return Err(PersistenceError::StalePreparedTasks.into());
        }
        let inherited = state
            .protocol
            .task_handoff
            .as_ref()
            .and_then(|handoff| handoff.plans.get(&(task_id.clone(), digest)))
            .ok_or(PreparationError::InvalidPreparedPlan)?;
        let mut plans = self.tasks.clone();
        if let Some(local) = plans.get_mut(task_id) {
            if local.candidate(digest)?.is_some() {
                return Ok(());
            }
            local.merge_witness(inherited)?;
        } else {
            let mut witness = inherited.clone();
            witness.commit_authorized = false;
            plans.insert(task_id.clone(), witness);
        }
        self.store.save_with_prepared(
            state,
            state,
            &snapshot.validator_set,
            &self.tasks,
            &plans,
        )?;
        self.tasks = plans;
        Ok(())
    }

    /// Validate a committee member's contribution and return the canonical union
    /// with local obligations. This only admits witnesses; it neither seals the
    /// result for membership consensus nor grants remote plans resource rights.
    /// The collection phase and witnesses are persisted in one transaction.
    pub fn collect_transition_handoff(
        &mut self,
        state: &mut SecondState,
        transition: &crate::ValidatorSetTransition,
        authorizers: &crate::AuthorizerSet,
        now: u64,
    ) -> Result<crate::ValidatorSetTransition, PreparationError> {
        self.stage_transition_handoff(state, transition, authorizers, now, true)
    }

    pub(crate) fn admit_transition_handoff(
        &mut self,
        state: &mut SecondState,
        transition: &crate::ValidatorSetTransition,
        authorizers: &crate::AuthorizerSet,
        now: u64,
    ) -> Result<(), PreparationError> {
        self.stage_transition_handoff(state, transition, authorizers, now, false)
            .map(|_| ())
    }

    fn stage_transition_handoff(
        &mut self,
        state: &mut SecondState,
        transition: &crate::ValidatorSetTransition,
        authorizers: &crate::AuthorizerSet,
        now: u64,
        collect: bool,
    ) -> Result<crate::ValidatorSetTransition, PreparationError> {
        let handoff = transition
            .handoff
            .as_ref()
            .ok_or(PreparationError::InvalidPreparedPlan)?;
        let snapshot = self
            .store
            .load()?
            .ok_or(PersistenceError::MissingSnapshot)?;
        let verified = crate::ValidatorSetTransitionSource::from_transition(transition)
            .verify(&snapshot.validator_set, &snapshot.validator_registry)
            .map_err(|_| PreparationError::InvalidPreparedPlan)?;
        if verified != *transition
            || transition.currency_frontier() != state.next_currency_address()
            || transition
                .clone()
                .with_handoff_arc(handoff.clone())?
                .digest()
                != transition.digest()
        {
            return Err(PreparationError::InvalidPreparedPlan);
        }
        handoff.validate_business_baseline(&snapshot)?;
        if !snapshot.state.same_persisted_state(state) || snapshot.prepared_tasks != self.tasks {
            return Err(PersistenceError::StalePreparedTasks.into());
        }
        let mut candidate_state = state.clone();
        handoff.admit_requests(&mut candidate_state, authorizers)?;
        let mut plans = self.tasks.clone();
        for ((task_id, digest), plan) in &handoff.plans {
            // Already admitted bodies keep their exact local rights and proofs.
            if plans
                .get(task_id)
                .map(|local| local.candidate(*digest))
                .transpose()?
                .flatten()
                .is_some()
                || state
                    .protocol
                    .task_handoff
                    .as_ref()
                    .is_some_and(|previous| {
                        previous.plans.contains_key(&(task_id.clone(), *digest))
                    })
            {
                continue;
            }
            if plan.validator_set_version != snapshot.validator_set.version() {
                return Err(PreparationError::TaskOriginMismatch {
                    expected: snapshot.validator_set.version(),
                    actual: plan.validator_set_version,
                });
            }
            let task = plan
                .source_task
                .clone()
                .verify(authorizers)
                .map_err(|_| PreparationError::InvalidPreparedPlan)?;
            let source = super::source::PreparedTaskSource::decode(&plan.encode_source()?)
                .ok_or(PreparationError::InvalidPreparedPlan)?;
            let mut isolated = Self {
                tasks: Default::default(),
                claims: CurrencyClaimBook::new(),
                lifecycle_claims: LifecycleClaimBook::default(),
                store: self.store.clone(),
            };
            let mut trial = state.clone();
            let BuildOutcome::Prepared(built) = isolated.build_prepared(
                &mut trial,
                &task,
                self.admitted_request_time(
                    state,
                    &task,
                    plan.validator_set_version,
                    Some(*digest),
                    now,
                ),
                plan.validator_set_version,
                Some(&source.selections),
            )?
            else {
                return Err(PreparationError::InvalidPreparedPlan);
            };
            if built.as_ref() != plan || built.plan_digest()? != *digest {
                return Err(PreparationError::InvalidPreparedPlan);
            }
            candidate_state.bind_task(&task)?;
            if let Some(local) = plans.get_mut(task_id) {
                local.merge_witness(plan)?;
            } else {
                // A validated handoff is bounded by its canonical encoded size,
                // not the unrelated unregistered-message queue window.
                let mut witness = plan.clone();
                witness.commit_authorized = false;
                plans.insert(task_id.clone(), witness);
            }
        }
        let mut staged = snapshot.clone();
        staged.state = candidate_state.clone();
        staged.prepared_tasks = plans.clone();
        let collected = if collect {
            // Capture uses the sole canonical normalization and includes every
            // inherited, owned and witness variant in the staged task book.
            transition
                .clone()
                .with_handoff(crate::persistence::TaskHandoff::capture(&staged)?)?
        } else {
            transition.clone()
        };
        if collect
            && handoff
                .plans
                .iter()
                .any(|(key, plan)| collected.handoff.as_ref().unwrap().plans.get(key) != Some(plan))
        {
            return Err(PreparationError::InvalidPreparedPlan);
        }
        if collect
            && handoff.requests.iter().any(|(task_id, task)| {
                let result = collected.handoff.as_ref().unwrap();
                result.requests.get(task_id) != Some(task)
                    && result
                        .task_context(task_id)
                        .is_none_or(|plan| plan.source_task != *task)
            })
        {
            return Err(PreparationError::InvalidPreparedPlan);
        }
        collected.handoff.as_ref().unwrap().covers(&staged)?;
        if collect || plans != self.tasks || !candidate_state.same_persisted_state(state) {
            self.store.save_with_prepared_and_collection(
                state,
                &candidate_state,
                &snapshot.validator_set,
                &self.tasks,
                &plans,
                collect.then_some(&collected),
            )?;
            self.tasks = plans;
            *state = candidate_state;
        }
        Ok(collected)
    }
}
