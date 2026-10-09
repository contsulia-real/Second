use super::{BuildOutcome, LifecycleClaimBook, PreparationError, PreparedTaskBook};
use crate::{CurrencyClaimBook, SecondState, ValidatorSet, VerifiedLegalTask};

pub(crate) struct VerifiedContention {
    pub(crate) prepared: crate::prepared_plan::PreparedTask,
    pub(crate) blockers: Vec<crate::TaskId>,
}

impl VerifiedContention {
    fn require_conflict(self) -> Result<Self, PreparationError> {
        if self.blockers.is_empty() {
            return Err(PreparationError::InvalidPreparedPlan);
        }
        Ok(self)
    }
}

impl crate::prepared_plan::PreparedTask {
    pub(crate) fn has_unavailable_transfer_address(&self, state: &SecondState) -> bool {
        self.source_task.payload().operations().iter().any(|operation| {
            matches!(operation, crate::Operation::Transfer { source, destination, .. }
                if state.payment_address_status(*source) != Some(crate::PaymentAddressStatus::Active)
                    || state.payment_address_status(*destination) != Some(crate::PaymentAddressStatus::Active))
        })
    }
}

impl PreparedTaskBook {
    pub(crate) fn mark_unusable_source_for_abort(
        &mut self,
        state: &SecondState,
        task: &VerifiedLegalTask,
        validators: &ValidatorSet,
        digest: [u8; 32],
        selections: &[Vec<crate::CurrencyAddress>],
    ) -> Result<bool, PreparationError> {
        let Some(existing) = self.tasks.get(&task.task_id()) else {
            return Ok(false);
        };
        if existing.phase == crate::prepared_plan::PreparedTaskPhase::Finalized
            || existing.finality_votes.is_some()
            || existing.request_digest != task.request_digest()
            || existing.source_task != *task.signed_task()
            || existing.validator_set_version != validators.version()
        {
            return Ok(false);
        }
        let Some(candidate) = existing.candidate(digest)? else {
            return Ok(false);
        };
        let source = super::source::PreparedTaskSource::decode(&candidate.encode_source()?)
            .ok_or(PreparationError::InvalidPreparedPlan)?;
        if source.selections != selections {
            return Ok(false);
        }
        let snapshot = self
            .store
            .load_shared()?
            .ok_or(crate::PersistenceError::MissingSnapshot)?;
        if crate::persistence::has_commit_evidence(&snapshot, &task.task_id()) {
            return Ok(false);
        }
        if existing.has_owned_candidate() {
            // An established holder can still execute on Retiring addresses.
            // Independently verify that the exact known source cannot establish
            // on a peer without that prerequisite; never delete the real one.
            let mut peer_state = state.clone();
            peer_state
                .prerequisite
                .payment_executions
                .retain(|claim, _| claim.task_id() != &task.task_id());
            if !matches!(
                self.verify_contention(&peer_state, task, 0, validators, digest, selections),
                Err(PreparationError::Execution(
                    crate::ExecutionError::PaymentAddressUnavailable(_)
                ))
            ) {
                return Ok(false);
            }
        }
        let persistence_set =
            self.persistence_validator_set(state, &task.task_id(), validators, Some(digest))?;
        let mut plans = self.tasks.clone();
        plans.get_mut(&task.task_id()).unwrap().conflict_abort = true;
        // This only enables the original task's Abort candidate. It creates no
        // payment prerequisite, ownership, terminal result or release.
        if plans != self.tasks {
            self.store
                .save_with_prepared(state, state, &persistence_set, &self.tasks, &plans)?;
            self.tasks = plans;
        }
        Ok(true)
    }

    /// Validate a frozen source independently of local occupancy, then consult
    /// the existing claim indexes. This grants neither claims nor voting rights.
    #[cfg(test)]
    pub(crate) fn validate_resource_conflict(
        &self,
        state: &SecondState,
        task: &VerifiedLegalTask,
        now: u64,
        validator_set: &ValidatorSet,
        expected_plan_digest: [u8; 32],
        selections: &[Vec<crate::CurrencyAddress>],
    ) -> Result<Vec<crate::TaskId>, PreparationError> {
        self.verify_contention(
            state,
            task,
            now,
            validator_set,
            expected_plan_digest,
            selections,
        )
        .map(|verified| verified.blockers)
    }

    pub(crate) fn verify_contention(
        &self,
        state: &SecondState,
        task: &VerifiedLegalTask,
        now: u64,
        validator_set: &ValidatorSet,
        expected_plan_digest: [u8; 32],
        selections: &[Vec<crate::CurrencyAddress>],
    ) -> Result<VerifiedContention, PreparationError> {
        self.verify_contention_inner(
            state,
            task,
            now,
            validator_set,
            Some(expected_plan_digest),
            Some(selections),
        )
        .and_then(VerifiedContention::require_conflict)
    }

    pub(crate) fn verify_local_contention(
        &self,
        state: &SecondState,
        task: &VerifiedLegalTask,
        now: u64,
        validator_set: &ValidatorSet,
    ) -> Result<VerifiedContention, PreparationError> {
        self.verify_contention_inner(state, task, now, validator_set, None, None)
            .and_then(VerifiedContention::require_conflict)
    }

    pub(crate) fn verify_certified_source(
        &self,
        state: &SecondState,
        task: &VerifiedLegalTask,
        now: u64,
        validators: &ValidatorSet,
        certificate: &crate::FinalityCertificate,
        selections: &[Vec<crate::CurrencyAddress>],
    ) -> Result<VerifiedContention, PreparationError> {
        certificate
            .verify(validators)
            .map_err(|_| PreparationError::InvalidPreparedPlan)?;
        let verified = self.verify_contention_inner(
            state,
            task,
            now,
            validators,
            Some(certificate.statement().subject_digest()),
            Some(selections),
        )?;
        if verified.prepared.plan_digest()? != certificate.statement().subject_digest() {
            return Err(PreparationError::InvalidPreparedPlan);
        }
        Ok(verified)
    }

    fn verify_contention_inner(
        &self,
        state: &SecondState,
        task: &VerifiedLegalTask,
        now: u64,
        validator_set: &ValidatorSet,
        expected_plan_digest: Option<[u8; 32]>,
        selections: Option<&[Vec<crate::CurrencyAddress>]>,
    ) -> Result<VerifiedContention, PreparationError> {
        if let Some(handoff) = &state.protocol.task_handoff {
            handoff.validate_task_origin(state, &task.task_id(), validator_set.version())?;
        }
        let now = self.admitted_request_time(
            state,
            task,
            validator_set.version(),
            expected_plan_digest,
            now,
        );
        let mut isolated = Self {
            tasks: Default::default(),
            claims: CurrencyClaimBook::new(),
            lifecycle_claims: LifecycleClaimBook::default(),
            store: self.store.clone(),
        };
        let mut candidate = state.clone();
        let BuildOutcome::Prepared(prepared) = isolated.build_prepared(
            &mut candidate,
            task,
            now,
            validator_set.version(),
            selections,
        )?
        else {
            return Err(PreparationError::InvalidPreparedPlan);
        };
        prepared.encode_source()?;
        let actual = prepared.plan_digest()?;
        // Abort binds the signed request and exact committee, rather than a
        // Commit plan. Its source still requires full business validation and
        // an actual intersecting local resource holder below.
        if let Some(expected_plan_digest) = expected_plan_digest
            && actual != expected_plan_digest
            && crate::task_abort::statement(&task.task_id(), task.request_digest(), validator_set)
                .subject_digest()
                != expected_plan_digest
        {
            return Err(PreparationError::PreparedPlanDigestMismatch {
                expected: expected_plan_digest,
                actual,
            });
        }
        let mut blockers = self.claims.conflicting_tasks(&isolated.claims);
        blockers.extend(
            self.lifecycle_claims
                .conflicting_tasks(&isolated.lifecycle_claims),
        );
        if let Some(handoff) = &state.protocol.task_handoff {
            blockers.extend(handoff.blockers(state, &prepared)?);
        }
        Ok(VerifiedContention {
            prepared: *prepared,
            blockers: blockers.into_iter().collect(),
        })
    }

    pub(crate) fn admit_contention(
        &mut self,
        state: &mut SecondState,
        task: &VerifiedLegalTask,
        validator_set: &ValidatorSet,
        verified: VerifiedContention,
    ) -> Result<Vec<crate::TaskId>, PreparationError> {
        let snapshot = self
            .store
            .load()?
            .ok_or(crate::PersistenceError::MissingSnapshot)?;
        let task_id = task.task_id();
        // Only an exact installed handoff candidate can retain old authority.
        // Reuse the same origin/persistence rule as ordinary frozen admission.
        let persistence_set = self.persistence_validator_set(
            state,
            &task_id,
            validator_set,
            Some(verified.prepared.plan_digest()?),
        )?;
        if snapshot.validator_set != persistence_set
            || verified.prepared.validator_set_version != validator_set.version()
        {
            return Err(PreparationError::TaskOriginMismatch {
                expected: snapshot.validator_set.version(),
                actual: validator_set.version(),
            });
        }
        let mut plans = self.tasks.clone();
        if let Some(existing) = plans.get_mut(&task_id) {
            existing.merge_witness(&verified.prepared)?;
        } else {
            if plans
                .values()
                .filter(|plan| !plan.commit_authorized)
                .count()
                >= crate::runtime_bft_consensus::MAX_PENDING_UNREGISTERED_SCOPES
            {
                return Err(crate::PersistenceError::SnapshotTooLarge.into());
            }
            let mut plan = verified.prepared;
            plan.commit_authorized = false;
            plans.insert(task_id.clone(), plan);
        }
        let mut losers = Vec::new();
        let candidate_protected = crate::persistence::has_commit_evidence(&snapshot, &task_id);
        let protected_blocker = verified
            .blockers
            .iter()
            .any(|other| crate::persistence::has_commit_evidence(&snapshot, other));
        if candidate_protected && protected_blocker {
            return Err(crate::PersistenceError::InvalidSnapshot.into());
        }
        if protected_blocker
            || (!candidate_protected && verified.blockers.iter().any(|other| other < &task_id))
        {
            plans.get_mut(&task_id).unwrap().conflict_abort = true;
            losers.push(task_id.clone());
        }
        for blocker in verified.blockers {
            if (candidate_protected || (!protected_blocker && blocker > task_id))
                && let Some(plan) = plans.get_mut(&blocker)
            {
                plan.conflict_abort = true;
                losers.push(blocker);
            }
        }
        let mut candidate = state.clone();
        candidate.bind_task(task)?;
        if !plans[&task_id].commit_authorized {
            candidate
                .prerequisite
                .payment_executions
                .retain(|claim, _| claim.task_id() != &task_id);
        }
        if !candidate.same_persisted_state(state) || plans != self.tasks {
            self.store.save_with_prepared(
                state,
                &candidate,
                &persistence_set,
                &self.tasks,
                &plans,
            )?;
        }
        self.tasks = plans;
        *state = candidate;
        Ok(losers)
    }
}
