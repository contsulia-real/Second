//! Event-scoped wakeups for validated contenders, never a resource owner.
use crate::{
    BftLocalState, BftPhase, BftQuorumCertificate, BftValue, ConsensusScope, PersistedNodeState,
    PersistenceError, StateStore, TaskId, ValidatorId,
};

pub(crate) fn has_commit_evidence(snapshot: &PersistedNodeState, task_id: &TaskId) -> bool {
    // Ambiguous finality must remain protected, never become Abort eligible.
    commit_evidence_digest(snapshot, task_id).map_or(true, |digest| digest.is_some())
}

pub(crate) fn handoff_evidence_digests(
    snapshot: &PersistedNodeState,
) -> Result<
    std::collections::BTreeMap<TaskId, std::collections::BTreeSet<[u8; 32]>>,
    PersistenceError,
> {
    let mut protected = std::collections::BTreeMap::<TaskId, std::collections::BTreeSet<_>>::new();
    for (task_id, plan) in &snapshot.prepared_tasks {
        if plan.phase == crate::prepared_plan::PreparedTaskPhase::Finalized
            || plan.finality_votes.is_some()
        {
            protected.entry(task_id.clone()).or_default().insert(
                plan.plan_digest()
                    .map_err(|_| PersistenceError::InvalidSnapshot)?,
            );
        }
        if snapshot
            .state
            .protocol
            .task_bindings
            .get(task_id)
            .is_some_and(|binding| binding.allocation_certificate.is_some())
        {
            let digests = plan
                .owned_candidate_digests()
                .and_then(|mut digests| {
                    digests.extend(plan.unowned_candidate_digests()?);
                    Ok(digests)
                })
                .map_err(|_| PersistenceError::InvalidSnapshot)?;
            protected
                .entry(task_id.clone())
                .or_default()
                .extend(digests);
        }
    }
    for ((_, scope), digest) in &snapshot.validator_vote_locks {
        if let ConsensusScope::PreparedTask(task_id) = scope {
            protected
                .entry(task_id.clone())
                .or_default()
                .insert(*digest);
        }
    }
    for ((_, scope), state) in &snapshot.bft_local_states {
        if let ConsensusScope::PreparedTask(task_id) = scope {
            let digests = [
                state.locked_digest(),
                state.finality_ready_digest(),
                state
                    .valid_prevote_qc()
                    .and_then(|qc| qc.statement().value().digest()),
                state.prevote().and_then(|value| value.digest()),
                state.precommit().and_then(|value| value.digest()),
            ];
            for digest in digests.into_iter().flatten() {
                protected.entry(task_id.clone()).or_default().insert(digest);
            }
        }
    }
    Ok(protected)
}

fn commit_evidence_digest(
    snapshot: &PersistedNodeState,
    task_id: &TaskId,
) -> Result<Option<[u8; 32]>, PersistenceError> {
    let Some(plan) = snapshot.prepared_tasks.get(task_id) else {
        return Ok(None);
    };
    let scope = ConsensusScope::PreparedTask(task_id.clone());
    let final_digests = snapshot
        .validator_vote_locks
        .iter()
        .filter(|((_, candidate), _)| candidate == &scope)
        .map(|(_, digest)| *digest)
        .chain(
            snapshot
                .bft_local_states
                .iter()
                .filter(|((_, candidate), _)| candidate == &scope)
                .filter_map(|(_, state)| state.finality_ready_digest()),
        )
        .chain(
            plan.finality_votes
                .as_ref()
                .map(|_| plan.plan_digest())
                .transpose()
                .map_err(|_| PersistenceError::InvalidSnapshot)?,
        );
    let mut final_choice = None;
    for final_digest in final_digests {
        if final_choice.is_some_and(|other| other != final_digest) {
            return Err(PersistenceError::InvalidSnapshot);
        }
        final_choice = Some(final_digest);
    }
    let choice = final_choice.or_else(|| {
        snapshot
            .bft_local_states
            .iter()
            .filter(|((_, candidate), _)| candidate == &scope)
            .filter_map(|(_, state)| state.valid_prevote_qc())
            .max_by_key(|qc| qc.statement().round())
            .and_then(|qc| qc.statement().value().digest())
    });
    choice
        .map(|digest| {
            plan.candidate(digest)
                .map(|candidate| candidate.map(|_| digest))
                .map_err(|_| PersistenceError::InvalidSnapshot)
        })
        .transpose()
        .map(Option::flatten)
}

impl StateStore {
    pub(crate) fn reconcile_prepared_commit_finality(
        &self,
        validator_id: ValidatorId,
        task_id: &TaskId,
        certificate: &crate::FinalityCertificate,
    ) -> Result<Vec<TaskId>, PersistenceError> {
        let _guard = self.lock()?;
        let mut latest = self
            .load_unlocked()?
            .ok_or(PersistenceError::MissingSnapshot)?;
        let Some(plan) = latest.prepared_tasks.get(task_id) else {
            return Ok(Vec::new());
        };
        let digest = certificate.statement().subject_digest();
        let Some(candidate) = plan
            .candidate(digest)
            .map_err(|_| PersistenceError::InvalidSnapshot)?
        else {
            return Err(PersistenceError::InvalidSnapshot);
        };
        if candidate.commit_authorized && !plan.conflict_abort {
            return Ok(Vec::new());
        }
        let validators = super::snapshot_validation::resolve_validator_set(
            &latest.validator_set,
            &latest.retained_validator_sets,
            plan.validator_set_version,
        )
        .ok_or(PersistenceError::InvalidSnapshot)?;
        certificate
            .verify(validators)
            .map_err(|_| PersistenceError::InvalidSnapshot)?;
        if !validators.contains(validator_id) {
            return Err(PersistenceError::ValidatorRegistryMismatch);
        }
        let key = (validator_id, ConsensusScope::PreparedTask(task_id.clone()));
        if latest
            .validator_vote_locks
            .get(&key)
            .is_some_and(|other| *other != digest)
        {
            return Err(PersistenceError::InvalidSnapshot);
        }
        let local = latest
            .bft_local_states
            .entry(key)
            .or_insert_with(|| BftLocalState::new(validators.version()));
        if local
            .finality_ready_digest()
            .is_some_and(|other| other != digest)
        {
            return Err(PersistenceError::InvalidSnapshot);
        }
        let mut changed = local.finality_ready_digest() != Some(digest);
        if changed {
            local.mark_finality_ready(local.round(), digest);
        }
        let (choices_changed, losers) = protect_plan(&mut latest, task_id, digest)?;
        changed |= choices_changed;
        if changed {
            self.write_local_metadata_unlocked(&latest)?;
        }
        Ok(losers)
    }
    /// Observe a verified Commit QC without granting resource or signing rights.
    /// The proof and resulting local choices share the existing snapshot write.
    pub(crate) fn reconcile_prepared_commit_qc(
        &self,
        validator_id: ValidatorId,
        certificate: &BftQuorumCertificate,
    ) -> Result<Vec<TaskId>, PersistenceError> {
        let _guard = self.lock()?;
        let mut latest = self
            .load_unlocked()?
            .ok_or(PersistenceError::MissingSnapshot)?;
        let statement = certificate.statement();
        let ConsensusScope::PreparedTask(task_id) = statement.scope() else {
            return Ok(Vec::new());
        };
        let Some(plan) = latest.prepared_tasks.get(task_id) else {
            return Ok(Vec::new());
        };
        let BftValue::Digest(digest) = statement.value() else {
            return Ok(Vec::new());
        };
        let Some(candidate) = plan
            .candidate(digest)
            .map_err(|_| PersistenceError::InvalidSnapshot)?
        else {
            return Ok(Vec::new());
        };
        if candidate.commit_authorized && !plan.conflict_abort {
            // Ordinary task QCs remain the BFT driver's single durable path.
            return Ok(Vec::new());
        }
        if statement.phase() != BftPhase::Prevote {
            return Ok(Vec::new());
        }
        let validators = super::snapshot_validation::resolve_validator_set(
            &latest.validator_set,
            &latest.retained_validator_sets,
            plan.validator_set_version,
        )
        .ok_or(PersistenceError::InvalidSnapshot)?;
        if statement.validator_set_version() != validators.version()
            || !validators.contains(validator_id)
        {
            return Err(PersistenceError::ValidatorRegistryMismatch);
        }
        certificate
            .verify(validators)
            .map_err(PersistenceError::Bft)?;
        let key = (validator_id, statement.scope().clone());
        if latest
            .validator_vote_locks
            .get(&key)
            .is_some_and(|other| *other != digest)
        {
            return Ok(Vec::new());
        }
        let local = latest
            .bft_local_states
            .entry(key)
            .or_insert_with(|| BftLocalState::new(validators.version()));
        if local
            .finality_ready_digest()
            .is_some_and(|digest| BftValue::Digest(digest) != statement.value())
            || local.valid_prevote_qc().is_some_and(|qc| {
                qc.statement().round() >= statement.round()
                    && qc.statement().value() != statement.value()
            })
        {
            return Ok(Vec::new());
        }
        let mut changed = false;
        if local.round() < statement.round() {
            local.set_round(statement.round());
            changed = true;
        }
        changed |= local.remember_prevote_qc(certificate);
        let (choices_changed, losers) = protect_plan(&mut latest, task_id, digest)?;
        changed |= choices_changed;
        if changed {
            self.write_local_metadata_unlocked(&latest)?;
        }
        Ok(losers)
    }
    pub(crate) fn contenders_for(&self, task_id: &TaskId) -> Result<Vec<TaskId>, PersistenceError> {
        let snapshot = self
            .load_shared()?
            .ok_or(PersistenceError::MissingSnapshot)?;
        let mut plans = snapshot.prepared_tasks.get(task_id).into_iter().chain(
            snapshot
                .state
                .protocol
                .task_handoff
                .as_ref()
                .into_iter()
                .flat_map(|handoff| {
                    handoff
                        .plans
                        .range((task_id.clone(), [0; 32])..=(task_id.clone(), [u8::MAX; 32]))
                        .map(|(_, plan)| plan)
                }),
        );
        let Some(first) = plans.next() else {
            return Ok(Vec::new());
        };
        let index = contender_index(&snapshot)?;
        let mut contenders = contenders_for_plan(&snapshot, first, &index)?;
        for plan in plans {
            contenders.extend(contenders_for_plan(&snapshot, plan, &index)?);
        }
        Ok(contenders.into_iter().collect())
    }
}

pub(crate) fn contenders_for_plan(
    snapshot: &PersistedNodeState,
    plan: &crate::prepared_plan::PreparedTask,
    index: &crate::prepared::fences::ResourceFenceIndex,
) -> Result<std::collections::BTreeSet<TaskId>, PersistenceError> {
    let mut tasks = index
        .blockers(&snapshot.state, plan)
        .map_err(|_| PersistenceError::InvalidSnapshot)?;
    // Retirement changes establishment eligibility, not a currency claim.
    // Both finality delivery and certified recovery use this terminal event.
    let retiring: std::collections::BTreeSet<_> = plan
        .source_task
        .payload()
        .operations()
        .iter()
        .filter_map(|operation| match operation {
            crate::Operation::RetirePaymentAddress { address } => Some(*address),
            _ => None,
        })
        .collect();
    if !retiring.is_empty() {
        tasks.extend(snapshot.prepared_tasks.iter().filter(|(id, candidate)| {
            **id != plan.task_id && candidate.source_task.payload().operations().iter().any(|operation|
                matches!(operation, crate::Operation::Transfer { source, destination, .. }
                    if retiring.contains(source) || retiring.contains(destination)))
        }).map(|(id, _)| id.clone()));
    }
    Ok(tasks)
}

pub(crate) fn contender_index(
    snapshot: &PersistedNodeState,
) -> Result<crate::prepared::fences::ResourceFenceIndex, PersistenceError> {
    crate::prepared::fences::ResourceFenceIndex::new(snapshot.prepared_tasks.values().filter(
        |plan| {
            !plan.commit_authorized
                || plan
                    .variants
                    .iter()
                    .any(|variant| !variant.commit_authorized)
        },
    ))
    .map_err(|_| PersistenceError::InvalidSnapshot)
}

fn protect_plan(
    latest: &mut PersistedNodeState,
    task_id: &TaskId,
    digest: [u8; 32],
) -> Result<(bool, Vec<TaskId>), PersistenceError> {
    let plan = latest
        .prepared_tasks
        .get_mut(task_id)
        .ok_or(PersistenceError::InvalidSnapshot)?;
    let mut changed =
        plan.phase == crate::prepared_plan::PreparedTaskPhase::Prepared || plan.conflict_abort;
    plan.advance_phase(crate::prepared_plan::PreparedTaskPhase::Voting);
    plan.conflict_abort = false;
    let mut selected = plan
        .candidate(digest)
        .map_err(|_| PersistenceError::InvalidSnapshot)?
        .ok_or(PersistenceError::InvalidSnapshot)?;
    // A quorum chose these operations, not every locally retained alternative.
    selected.variants.clear();
    let index = crate::prepared::fences::ResourceFenceIndex::new(latest.prepared_tasks.values())
        .map_err(|_| PersistenceError::InvalidSnapshot)?;
    let blockers = index
        .blockers(&latest.state, &selected)
        .map_err(|_| PersistenceError::InvalidSnapshot)?;
    let selected_index =
        crate::prepared::fences::ResourceFenceIndex::new(std::iter::once(&selected))
            .map_err(|_| PersistenceError::InvalidSnapshot)?;
    let mut losers = Vec::new();
    for blocker in blockers {
        if let Some(protected_digest) = commit_evidence_digest(latest, &blocker)? {
            let mut protected = latest.prepared_tasks[&blocker]
                .candidate(protected_digest)
                .map_err(|_| PersistenceError::InvalidSnapshot)?
                .ok_or(PersistenceError::InvalidSnapshot)?;
            protected.variants.clear();
            if !selected_index
                .blockers(&latest.state, &protected)
                .map_err(|_| PersistenceError::InvalidSnapshot)?
                .is_empty()
            {
                return Err(PersistenceError::InvalidSnapshot);
            }
            // Only an unused retained candidate overlaps. Keep its claims until
            // this protected task commits, then wake the waiting contender.
            continue;
        }
        let plan = latest.prepared_tasks.get_mut(&blocker).unwrap();
        changed |= !plan.conflict_abort;
        plan.conflict_abort = true;
        losers.push(blocker);
    }
    Ok((changed, losers))
}
