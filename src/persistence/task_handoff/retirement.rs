//! Local rights retire only inside the atomic installation of a verified cut.
use super::*;

#[cfg(test)]
mod tests;

pub(in crate::persistence) fn hydrate_certified(
    snapshot: &mut PersistedNodeState,
    transition: &crate::ValidatorSetTransition,
) -> Result<crate::ValidatorSetTransition, PersistenceError> {
    // The caller verifies the old quorum first, holding the store write lock.
    // Resolve and authenticate the exact body before touching the staged state.
    let candidate = resolve(snapshot, transition)?;
    let handoff = candidate.handoff.as_ref().unwrap();
    handoff.validate_business_baseline(snapshot)?;
    let protected = super::super::contention::handoff_evidence_digests(snapshot)?;
    handoff.covers_evidence(snapshot, &protected)?;
    let mut omitted = Vec::new();
    for (task_id, plan) in &snapshot.prepared_tasks {
        let mut obligations = TaskHandoff::default();
        obligations.insert(plan.clone())?;
        if obligations
            .plans
            .keys()
            .all(|key| handoff.plans.contains_key(key))
        {
            continue;
        }
        omitted.push(task_id.clone());
    }
    for task_id in omitted {
        let plan = snapshot.prepared_tasks.get_mut(&task_id).unwrap();
        if handoff.task_context(&task_id).is_some() {
            // Covered candidates retain their rights. Omitted variants remain
            // exact witnesses, but cannot supply old Commit signatures or claims.
            if !handoff.plans.contains_key(&(
                task_id.clone(),
                plan.plan_digest()
                    .map_err(|_| PersistenceError::InvalidSnapshot)?,
            )) {
                plan.commit_authorized = false;
            }
            for index in 0..plan.variants.len() {
                let digest = plan
                    .operations_digest(&plan.variants[index].operations)
                    .map_err(|_| PersistenceError::InvalidSnapshot)?;
                if !handoff.plans.contains_key(&(task_id.clone(), digest)) {
                    plan.variants[index].commit_authorized = false;
                }
            }
            if !plan.commit_authorized
                && let Some(digest) = plan
                    .owned_candidate_digests()
                    .map_err(|_| PersistenceError::InvalidSnapshot)?
                    .first()
            {
                plan.select_candidate(*digest)
                    .map_err(|_| PersistenceError::InvalidSnapshot)?;
            }
            if !plan.has_owned_candidate() {
                snapshot
                    .state
                    .prerequisite
                    .payment_executions
                    .retain(|claim, _| claim.task_id() != &task_id);
            }
            continue;
        }
        let plan = snapshot.prepared_tasks.remove(&task_id).unwrap();
        let binding = snapshot
            .state
            .protocol
            .task_bindings
            .get_mut(&task_id)
            .ok_or(PersistenceError::InvalidSnapshot)?;
        if binding.request_digest != plan.request_digest
            || binding
                .allocation_task
                .as_ref()
                .is_some_and(|saved| saved != &plan.source_task)
        {
            return Err(PersistenceError::InvalidSnapshot);
        }
        // Reuse the durable request queue; never discard the original signature
        // or reset a certified continuous address range.
        binding.allocation_task = Some(plan.source_task);
        snapshot
            .state
            .prerequisite
            .payment_executions
            .retain(|claim, _| claim.task_id() != &task_id);
        snapshot
            .bft_local_states
            .retain(|(_, scope), _| scope != &crate::ConsensusScope::PreparedTask(task_id.clone()));
    }
    // Only the verified cut makes this exact root admitted in the staged view.
    // The writer drops its old-committee pending entry as it installs the new set.
    snapshot.pending_governance.insert(
        candidate.digest(),
        super::super::PendingGovernance::Transition(candidate.clone()),
    );
    // The original strict hydrator still checks every advertised body and all
    // inherited certified duties. Failure leaves the disk generation unchanged.
    hydrate(snapshot, &candidate)
}

impl TaskHandoff {
    pub(super) fn covers_evidence(
        &self,
        snapshot: &PersistedNodeState,
        protected: &BTreeMap<TaskId, std::collections::BTreeSet<[u8; 32]>>,
    ) -> Result<(), PersistenceError> {
        let mut abort_contexts = BTreeMap::new();
        for (task_id, digests) in protected {
            let Some(plan) = snapshot.prepared_tasks.get(task_id) else {
                continue;
            };
            let validators = super::super::snapshot_validation::resolve_validator_set(
                &snapshot.validator_set,
                &snapshot.retained_validator_sets,
                plan.validator_set_version,
            )
            .ok_or(PersistenceError::ValidatorRegistryMismatch)?;
            let abort = abort_contexts
                .entry(validators.version())
                .or_insert_with(|| crate::task_abort::StatementContext::new(validators))
                .statement(task_id, plan.request_digest)
                .subject_digest();
            for digest in digests {
                let included = if *digest == abort {
                    self.task_context(task_id).is_some_and(|candidate| {
                        candidate.validator_set_version == plan.validator_set_version
                            && candidate.request_digest == plan.request_digest
                    })
                } else {
                    self.plans.contains_key(&(task_id.clone(), *digest))
                };
                if !included {
                    return Err(PersistenceError::StalePreparedTasks);
                }
            }
        }
        Ok(())
    }
}
