//! Execute a dependency-closed set of certified choices in one business write.
use super::fences::ResourceFenceIndex;
use super::*;
use std::collections::BTreeSet;

#[cfg(test)]
#[path = "certified_tests.rs"]
mod tests;

#[derive(Default)]
pub(crate) struct CertifiedCommitOutcome {
    pub(crate) completed: Vec<TaskId>,
    pub(crate) retry: BTreeSet<TaskId>,
}

impl PreparedTaskBook {
    /// Missing terminal evidence leaves all resources and business state intact.
    /// Certificates came through the normal snapshot verifier; no new votes or
    /// temporary resource grants are needed to execute their exact choices.
    pub(crate) fn commit_certified_component(
        store: &StateStore,
        root: &TaskId,
        expected_state: Option<&SecondState>,
    ) -> Result<CertifiedCommitOutcome, PreparationError> {
        for attempt in 0..=3 {
            match Self::commit_certified_component_once(store, root, expected_state) {
                Err(PreparationError::Persistence(
                    PersistenceError::StaleState | PersistenceError::StalePreparedTasks,
                )) if attempt < 3 => continue,
                result => return result,
            }
        }
        unreachable!("bounded stale retry")
    }

    fn commit_certified_component_once(
        store: &StateStore,
        root: &TaskId,
        expected_state: Option<&SecondState>,
    ) -> Result<CertifiedCommitOutcome, PreparationError> {
        let snapshot = store
            .load_shared()?
            .ok_or(PersistenceError::MissingSnapshot)?;
        if expected_state.is_some_and(|expected| !snapshot.state.same_persisted_state(expected)) {
            // Preserve the established execution error for an invalid caller
            // state; a merely stale but executable state still reports StaleState.
            if let Some(plan) = snapshot.prepared_tasks.get(root) {
                let mut trial = expected_state.unwrap().clone();
                Self::commit_inner(&mut trial, plan)?;
            }
            return Err(PersistenceError::StaleState.into());
        }
        if !snapshot.prepared_tasks.contains_key(root) {
            return Ok(CertifiedCommitOutcome::default());
        }
        let owned = ResourceFenceIndex::owned(snapshot.prepared_tasks.values())?;
        let certified = snapshot
            .prepared_tasks
            .values()
            .filter(|plan| plan.phase == PreparedTaskPhase::Finalized)
            .map(|plan| {
                let mut selected = plan.clone();
                selected.variants.clear();
                selected
            })
            .collect::<Vec<_>>();
        let certified_index = ResourceFenceIndex::new(certified.iter())?;
        let mut pending = BTreeSet::from([root.clone()]);
        let mut component = BTreeMap::new();
        while let Some(task_id) = pending.pop_first() {
            if component.contains_key(&task_id) {
                continue;
            }
            let Some(plan) = snapshot
                .prepared_tasks
                .get(&task_id)
                .filter(|plan| plan.phase == PreparedTaskPhase::Finalized)
            else {
                return Ok(CertifiedCommitOutcome::default());
            };
            let mut selected = plan.clone();
            selected.variants.clear();
            let mut blockers = owned.blockers(&snapshot.state, &selected)?;
            blockers.extend(certified_index.blockers(&snapshot.state, &selected)?);
            if let Some(handoff) = &snapshot.state.protocol.task_handoff {
                blockers.extend(handoff.blockers(&snapshot.state, &selected)?);
            }
            pending.extend(
                blockers
                    .into_iter()
                    .filter(|task| !component.contains_key(task)),
            );
            component.insert(task_id, selected);
        }
        // Even validly signed but contradictory task decisions cannot execute
        // overlapping resources twice (including transfers back to their owner).
        let mut claims = CurrencyClaimBook::new();
        let mut lifecycle = LifecycleClaimBook::default();
        for plan in component.values() {
            plan.restore_claims(&mut claims)?;
            plan.restore_lifecycle_claims(&mut lifecycle)?;
        }
        let contenders = crate::persistence::contender_index(&snapshot)?;
        let mut retry = BTreeSet::new();
        for task_id in component.keys() {
            retry.extend(crate::persistence::contenders_for_plan(
                &snapshot,
                &snapshot.prepared_tasks[task_id],
                &contenders,
            )?);
        }
        retry.retain(|task| !component.contains_key(task));
        let mut state = snapshot.state.clone();
        let mut remaining = snapshot.prepared_tasks.clone();
        for (task_id, plan) in &component {
            Self::commit_inner(&mut state, plan)?;
            remaining.remove(task_id);
        }
        store.commit_prepared_state(
            &snapshot.state,
            &state,
            &snapshot.prepared_tasks,
            &remaining,
        )?;
        Ok(CertifiedCommitOutcome {
            completed: component.into_keys().collect(),
            retry,
        })
    }
}
