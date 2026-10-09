//! Sequential consensus storage work, isolated from the network executor.
use super::*;
use std::sync::Arc;

#[cfg(test)]
mod tests;

impl NodeRuntime {
    pub(crate) async fn run_validator_bft_consensus(
        self: &Arc<Self>,
    ) -> Result<(), NodeRuntimeError> {
        let Some(runtime) = self.validator_bft.as_ref() else {
            return std::future::pending::<Result<(), NodeRuntimeError>>().await;
        };
        let node = Arc::clone(self);
        let mut resumed_state = tokio::task::spawn_blocking(move || node.start_consensus_runner())
            .await
            .map_err(NodeRuntimeError::RuntimeTaskFailed)??;
        // Unrelated notifications do not reset the absolute source retry budget.
        let mut sync_retry_at = add_duration(Instant::now(), runtime.bft_timeouts().proposal);
        loop {
            let node = Arc::clone(self);
            let (step, state) = tokio::task::spawn_blocking(move || {
                let mut state = resumed_state;
                let step = node.drive_consensus_once(&mut state)?;
                Ok::<_, NodeRuntimeError>((step, state))
            })
            .await
            .map_err(NodeRuntimeError::RuntimeTaskFailed)??;
            let (pending_sync, deadline) = step;
            resumed_state = state;
            if runtime
                .consensus()
                .wait_for_activity_or_deadline(deadline, pending_sync.then_some(sync_retry_at))
                .await
                && Instant::now() >= sync_retry_at
            {
                let node = Arc::clone(self);
                tokio::task::spawn_blocking(move || {
                    node.validator_bft
                        .as_ref()
                        .unwrap()
                        .retry_prepared_task_sync(true);
                    node.announce_pending_transition_sources()
                })
                .await
                .map_err(NodeRuntimeError::RuntimeTaskFailed)??;
                sync_retry_at = add_duration(Instant::now(), runtime.bft_timeouts().proposal);
            } else if !pending_sync {
                sync_retry_at = add_duration(Instant::now(), runtime.bft_timeouts().proposal);
            }
        }
    }

    fn start_consensus_runner(&self) -> Result<(u64, bool, u64), NodeRuntimeError> {
        let runtime = self
            .validator_bft
            .as_ref()
            .ok_or(NodeRuntimeError::ValidatorBftNotConfigured)?;
        self.recover_validator_safety_if_ready(runtime)?;
        // Capture before resuming: an external activation racing this work
        // must still be observed on the next existing consensus wake.
        let snapshot = self
            .full_store()?
            .load_shared()?
            .ok_or(NodeRuntimeError::SnapshotMissing)?;
        let state = (
            snapshot.validator_set.version(),
            snapshot.validator_safety_ready,
            snapshot.state.next_currency_address(),
        );
        self.start_durable_prepared_consensus()?;
        self.resume_inherited_prepared_sources()?;
        self.resume_currency_allocations()?;
        self.resume_public_checkpoint()?;
        self.resume_governance()?;
        Ok(state)
    }

    fn refresh_durable_work(
        &self,
        runtime: &ValidatorBftRuntime,
        resumed: &mut (u64, bool, u64),
    ) -> Result<(), NodeRuntimeError> {
        let snapshot = self
            .full_store()?
            .load_shared()?
            .ok_or(NodeRuntimeError::SnapshotMissing)?;
        let current = (
            snapshot.validator_set.version(),
            snapshot.validator_safety_ready,
            snapshot.state.next_currency_address(),
        );
        if *resumed == current {
            return Ok(());
        }
        let membership_changed = (resumed.0, resumed.1) != (current.0, current.1);
        // This is only a work cursor; the durable snapshot remains authority.
        // Capture before callbacks so a concurrent later change is not skipped.
        *resumed = current;
        if membership_changed {
            runtime.refresh_authority()?;
            runtime.consensus().retire_certified_omissions(&snapshot)?;
            self.resume_inherited_prepared_sources()?;
            self.resume_public_checkpoint()?;
            self.resume_governance()?;
        }
        runtime.retire_superseded_allocation_sync(current.0, current.2);
        self.resume_currency_allocations()?;
        Ok(())
    }

    fn drive_consensus_once(
        &self,
        resumed_state: &mut (u64, bool, u64),
    ) -> Result<(bool, Option<Instant>), NodeRuntimeError> {
        let runtime = self
            .validator_bft
            .as_ref()
            .ok_or(NodeRuntimeError::ValidatorBftNotConfigured)?;
        self.refresh_durable_work(runtime, resumed_state)?;
        let inbound = self.process_governance_bft_sources(runtime.drain_inbound());
        let inbound = self.process_prepared_task_sync(inbound)?;
        self.advance_transition_collections()?;
        let output = runtime
            .consensus()
            .drive(inbound, tokio::time::Instant::now());
        if !output.completed_prepared_tasks.is_empty() {
            let snapshot = self
                .full_store()?
                .load_shared()?
                .ok_or(NodeRuntimeError::SnapshotMissing)?;
            runtime.consensus().restore_completed_tasks_for(
                &snapshot,
                runtime.bft_timeouts().precommit,
                output.completed_prepared_tasks.iter(),
                true,
            )?;
        }
        for task_id in output.completed_prepared_tasks {
            runtime.finish_prepared_task_sync(&ConsensusScope::PreparedTask(task_id));
        }
        for task_id in output.retry_prepared_tasks {
            self.retry_contender(&task_id)?;
        }
        for message in output.outbound {
            let scope = message.scope().clone();
            let failures = runtime.broadcast(&message);
            if !failures.is_empty() {
                runtime.consensus().record_send_failures(scope, failures);
            }
        }
        if output.validator_set_changed
            || output.currency_allocation_committed
            || output.certified_recovery_checkpoint.is_some()
        {
            self.refresh_durable_work(runtime, resumed_state)?;
        }
        if let Some(checkpoint) = output.certified_recovery_checkpoint {
            let snapshot = self
                .full_store()?
                .load_shared()?
                .ok_or(NodeRuntimeError::SnapshotMissing)?;
            if snapshot
                .recovery_checkpoint_proof
                .as_ref()
                .is_some_and(|proof| proof.checkpoint() == checkpoint.checkpoint())
            {
                match self.publish_state_recovery_provider(&checkpoint) {
                    Ok(()) => {}
                    Err(NodeRuntimeError::Persistence(
                        PersistenceError::RecoveryCheckpointDoesNotMatchState,
                    )) => {}
                    Err(error) => return Err(error),
                }
            }
            self.refresh_recovery_candidates()?;
        }
        if output.business_state_changed || output.currency_allocation_committed {
            if output.business_state_changed && !output.currency_allocation_committed {
                // A certified business dependency may unblock a retained signed
                // request without advancing its already allocated address range.
                self.resume_currency_allocations()?;
            }
            self.refresh_recovery_candidates()?;
        }
        let pending_sync =
            runtime.has_pending_prepared_task_sync() || self.has_pending_transition_sources()?;
        Ok((pending_sync, runtime.consensus().next_deadline()))
    }
}
