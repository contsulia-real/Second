//! One signed request, bounded frozen candidates, and retained resource rights.
use super::*;
use crate::prepared_plan::PreparedVariant;

impl PreparedTask {
    pub(crate) fn merge_witness(&mut self, candidate: &Self) -> Result<(), PreparationError> {
        if self.request_digest != candidate.request_digest
            || self.source_task != candidate.source_task
            || self.validator_set_version != candidate.validator_set_version
        {
            return Err(PreparationError::InvalidPreparedPlan);
        }
        if self.candidate(candidate.plan_digest()?)?.is_some() {
            return Ok(());
        }
        // Authenticated handoff witnesses are bounded by the canonical body
        // and snapshot byte limits, not the unrelated pending-message window.
        self.variants.push(PreparedVariant {
            operations: candidate.operations.clone(),
            commit_authorized: false,
        });
        Ok(())
    }
    pub(crate) fn has_owned_candidate(&self) -> bool {
        self.commit_authorized
            || self
                .variants
                .iter()
                .any(|variant| variant.commit_authorized)
    }

    pub(crate) fn candidate(&self, digest: [u8; 32]) -> Result<Option<Self>, PreparationError> {
        if self.plan_digest()? == digest {
            return Ok(Some(self.clone()));
        }
        for variant in &self.variants {
            if self.operations_digest(&variant.operations)? == digest {
                let mut candidate = Self::from_persisted(
                    self.task_id.clone(),
                    self.request_digest,
                    self.source_task.clone(),
                    self.validator_set_version,
                    self.phase,
                    self.finality_votes.clone(),
                    variant.operations.clone(),
                );
                candidate.commit_authorized = variant.commit_authorized;
                candidate.conflict_abort = self.conflict_abort;
                return Ok(Some(candidate));
            }
        }
        Ok(None)
    }

    pub(crate) fn owned_candidate_digests(&self) -> Result<Vec<[u8; 32]>, PreparationError> {
        self.candidate_digests(true)
    }

    pub(crate) fn unowned_candidate_digests(&self) -> Result<Vec<[u8; 32]>, PreparationError> {
        self.candidate_digests(false)
    }

    fn candidate_digests(&self, owned: bool) -> Result<Vec<[u8; 32]>, PreparationError> {
        let mut digests = Vec::new();
        if self.commit_authorized == owned {
            digests.push(self.plan_digest()?);
        }
        for variant in self
            .variants
            .iter()
            .filter(|variant| variant.commit_authorized == owned)
        {
            digests.push(self.operations_digest(&variant.operations)?);
        }
        digests.sort_unstable();
        Ok(digests)
    }

    pub(crate) fn select_candidate(&mut self, digest: [u8; 32]) -> Result<(), PreparationError> {
        if self.plan_digest()? == digest {
            return Ok(());
        }
        if self.finality_votes.is_some() {
            return Err(PreparationError::InvalidPreparedPlan);
        }
        let candidate = self
            .candidate(digest)?
            .ok_or(PreparationError::InvalidPreparedPlan)?;
        let mut selected = None;
        for (index, variant) in self.variants.iter().enumerate() {
            if variant.operations == candidate.operations {
                selected = Some(index);
                break;
            }
        }
        let variant = self
            .variants
            .remove(selected.ok_or(PreparationError::InvalidPreparedPlan)?);
        let previous = PreparedVariant {
            operations: std::mem::replace(&mut self.operations, variant.operations),
            commit_authorized: std::mem::replace(
                &mut self.commit_authorized,
                variant.commit_authorized,
            ),
        };
        self.variants.push(previous);
        Ok(())
    }

    pub(crate) fn restore_owned_claims(
        &self,
        claims: &mut CurrencyClaimBook,
    ) -> Result<(), PreparationError> {
        self.restore_candidate_claims(claims, true)
    }

    pub(crate) fn restore_owned_lifecycle_claims(
        &self,
        claims: &mut LifecycleClaimBook,
    ) -> Result<(), PreparationError> {
        self.restore_candidate_lifecycle_claims(claims, true)
    }

    pub(crate) fn restore_candidate_claims(
        &self,
        claims: &mut CurrencyClaimBook,
        owned_only: bool,
    ) -> Result<(), PreparationError> {
        for (variant_index, (operations, owned)) in
            std::iter::once((&self.operations, self.commit_authorized))
                .chain(
                    self.variants
                        .iter()
                        .map(|variant| (&variant.operations, variant.commit_authorized)),
                )
                .enumerate()
        {
            if owned_only && !owned {
                continue;
            }
            if operations.len() != self.operations.len() {
                return Err(PreparationError::InvalidPreparedPlan);
            }
            for (index, operation) in operations.iter().enumerate() {
                let position = variant_index
                    .checked_mul(self.operations.len())
                    .and_then(|start| start.checked_add(index))
                    .ok_or(PreparationError::OperationIndexOverflow)?;
                operation.restore_claim(
                    OperationClaimId::new(
                        self.task_id.clone(),
                        u64::try_from(position)
                            .map_err(|_| PreparationError::OperationIndexOverflow)?,
                    ),
                    claims,
                )?;
            }
        }
        Ok(())
    }

    pub(crate) fn restore_candidate_lifecycle_claims(
        &self,
        claims: &mut LifecycleClaimBook,
        owned_only: bool,
    ) -> Result<(), PreparationError> {
        for (operations, owned) in std::iter::once((&self.operations, self.commit_authorized))
            .chain(
                self.variants
                    .iter()
                    .map(|variant| (&variant.operations, variant.commit_authorized)),
            )
        {
            if owned_only && !owned {
                continue;
            }
            for operation in operations {
                if let Some(address) = operation.lifecycle_address() {
                    claims.claim(self.task_id.clone(), address)?;
                }
            }
        }
        Ok(())
    }
}

impl PreparedTaskBook {
    /// Preserve active membership while admitting an exact certified old body.
    pub(super) fn persistence_validator_set(
        &self,
        state: &SecondState,
        task_id: &TaskId,
        origin: &ValidatorSet,
        expected_plan_digest: Option<[u8; 32]>,
    ) -> Result<ValidatorSet, PreparationError> {
        let inherited = expected_plan_digest.and_then(|digest| {
            state
                .protocol
                .task_handoff
                .as_ref()
                .and_then(|handoff| handoff.plans.get(&(task_id.clone(), digest)))
        });
        if inherited.is_none_or(|plan| plan.validator_set_version != origin.version()) {
            return Ok(origin.clone());
        }
        let snapshot = self
            .store
            .load_shared()?
            .ok_or(PersistenceError::MissingSnapshot)?;
        if snapshot.state.protocol.task_handoff != state.protocol.task_handoff {
            return Err(PersistenceError::StaleState.into());
        }
        if snapshot.validator_set != *origin
            && snapshot.retained_validator_sets.get(&origin.version()) != Some(origin)
        {
            return Err(PersistenceError::ValidatorRegistryMismatch.into());
        }
        Ok(snapshot.validator_set.clone())
    }

    /// A locally admitted request or exact certified handoff candidate keeps
    /// its original time eligibility.
    /// Pending bindings or remote committee hints alone are not admission proof.
    pub(super) fn admitted_request_time(
        &self,
        state: &SecondState,
        task: &VerifiedLegalTask,
        validator_set_version: u64,
        expected_plan_digest: Option<[u8; 32]>,
        now: u64,
    ) -> u64 {
        let matches_request = |plan: &PreparedTask| {
            plan.request_digest == task.request_digest()
                && plan.source_task == *task.signed_task()
                && plan.validator_set_version == validator_set_version
        };
        let local = self.tasks.get(&task.task_id()).is_some_and(matches_request);
        // The installed committee certificate attests this exact frozen body.
        // A binding, remote hint, or unlisted candidate cannot extend expiry.
        let certified = expected_plan_digest.is_some_and(|digest| {
            state
                .protocol
                .task_handoff
                .as_ref()
                .and_then(|handoff| handoff.plans.get(&(task.task_id(), digest)))
                .is_some_and(matches_request)
        });
        if local || certified { 0 } else { now }
    }

    pub(crate) fn admit_frozen_variant(
        &mut self,
        state: &mut SecondState,
        task: &VerifiedLegalTask,
        validators: &ValidatorSet,
        digest: [u8; 32],
        selections: &[Vec<crate::CurrencyAddress>],
    ) -> Result<Option<super::conflict::VerifiedContention>, PreparationError> {
        let existing = self
            .tasks
            .get(&task.task_id())
            .ok_or(PreparationError::InvalidPreparedPlan)?;
        if existing.request_digest != task.request_digest()
            || existing.source_task != *task.signed_task()
            || existing.validator_set_version != validators.version()
            || existing.finality_votes.is_some()
        {
            return Err(PreparationError::InvalidPreparedPlan);
        }
        let known = existing.candidate(digest)?;
        if known.as_ref().is_some_and(|plan| plan.commit_authorized) {
            return Ok(None);
        }
        if let Some(handoff) = &state.protocol.task_handoff {
            handoff.validate_task_origin(state, &task.task_id(), validators.version())?;
        }
        if known.is_none()
            && existing.variants.len() + 1
                >= crate::runtime_bft_consensus::MAX_PENDING_MESSAGES_PER_SCOPE - 1
        {
            return Err(PersistenceError::SnapshotTooLarge.into());
        }
        let mut isolated = Self {
            tasks: Default::default(),
            claims: CurrencyClaimBook::new(),
            lifecycle_claims: LifecycleClaimBook::default(),
            store: self.store.clone(),
        };
        let mut candidate_state = state.clone();
        let BuildOutcome::Prepared(candidate) = isolated.build_prepared(
            &mut candidate_state,
            task,
            0,
            validators.version(),
            Some(selections),
        )?
        else {
            return Err(PreparationError::InvalidPreparedPlan);
        };
        if candidate.plan_digest()? != digest {
            return Err(PreparationError::InvalidPreparedPlan);
        }
        candidate.encode_source()?;
        let mut blockers = self.claims.conflicting_tasks(&isolated.claims);
        blockers.extend(
            self.lifecycle_claims
                .conflicting_tasks(&isolated.lifecycle_claims),
        );
        if let Some(handoff) = &state.protocol.task_handoff {
            blockers.extend(handoff.blockers(state, &candidate)?);
        }
        if !blockers.is_empty() {
            return Ok(Some(super::conflict::VerifiedContention {
                prepared: *candidate,
                blockers: blockers.into_iter().collect(),
            }));
        }
        let mut plans = self.tasks.clone();
        let plan = plans.get_mut(&task.task_id()).unwrap();
        if plan.operations == candidate.operations {
            plan.commit_authorized = true;
        } else if let Some(variant) = plan
            .variants
            .iter_mut()
            .find(|variant| variant.operations == candidate.operations)
        {
            variant.commit_authorized = true;
        } else {
            plan.variants.push(PreparedVariant {
                operations: candidate.operations,
                commit_authorized: true,
            });
        }
        let snapshot = self
            .store
            .load()?
            .ok_or(PersistenceError::MissingSnapshot)?;
        if !crate::persistence::has_commit_evidence(&snapshot, &task.task_id()) {
            let minimum = plan
                .owned_candidate_digests()?
                .into_iter()
                .next()
                .ok_or(PreparationError::InvalidPreparedPlan)?;
            plan.select_candidate(minimum)?;
        }
        let mut claims = CurrencyClaimBook::new();
        let mut lifecycle = LifecycleClaimBook::default();
        for plan in plans.values() {
            plan.restore_owned_claims(&mut claims)?;
            plan.restore_owned_lifecycle_claims(&mut lifecycle)?;
        }
        let persistence_set =
            self.persistence_validator_set(state, &task.task_id(), validators, Some(digest))?;
        self.store.save_with_prepared(
            state,
            &candidate_state,
            &persistence_set,
            &self.tasks,
            &plans,
        )?;
        self.tasks = plans;
        self.claims = claims;
        self.lifecycle_claims = lifecycle;
        *state = candidate_state;
        Ok(None)
    }
}
