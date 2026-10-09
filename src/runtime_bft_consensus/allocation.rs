use super::*;

#[cfg(test)]
mod tests;

impl ValidatorConsensusRuntime {
    pub(crate) fn allocation_source(
        &self,
        scope: &ConsensusScope,
        digest: [u8; 32],
    ) -> Option<crate::LegalTask> {
        let coordinator = self
            .coordinator
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(completed) = coordinator.recent_completed.iter().find(|completed| {
            completed.subject.scope() == scope && completed.subject.digest() == digest
        }) {
            return completed.allocation_source.clone();
        }
        let target = coordinator.sessions.get(scope)?.candidates.get(&digest)?;
        match target {
            ValidatorConsensusTarget::CurrencyAllocation(allocation) => {
                Some(allocation.task.clone())
            }
            _ => None,
        }
    }
    pub(crate) fn allocation_transition_source(
        &self,
        scope: &ConsensusScope,
        digest: [u8; 32],
    ) -> Option<Vec<u8>> {
        let coordinator = self
            .coordinator
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(completed) = coordinator.recent_completed.iter().find(|completed| {
            completed.subject.scope() == scope && completed.subject.digest() == digest
        }) {
            return completed.transition_source.clone();
        }
        match coordinator.sessions.get(scope)?.candidates.get(&digest)? {
            ValidatorConsensusTarget::ValidatorSetTransition(transition) => {
                ValidatorSetTransitionSource::from_transition(transition)
                    .encode_bytes()
                    .ok()
            }
            _ => None,
        }
    }
    pub(crate) fn has_allocation_candidate(
        &self,
        scope: &ConsensusScope,
        digest: [u8; 32],
    ) -> bool {
        let coordinator = self
            .coordinator
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        coordinator
            .sessions
            .get(scope)
            .is_some_and(|session| session.candidates.contains_key(&digest))
            || coordinator.recent_completed.iter().any(|completed| {
                completed.subject.scope() == scope && completed.subject.digest() == digest
            })
    }
}

impl NodeRuntime {
    pub(crate) fn resume_currency_allocations(&self) -> Result<(), NodeRuntimeError> {
        let Some(runtime) = &self.validator_bft else {
            return Ok(());
        };
        let store = self.full_store()?;
        let snapshot = store
            .load_shared()?
            .ok_or(NodeRuntimeError::SnapshotMissing)?;
        let mut pending = snapshot
            .state
            .protocol
            .task_bindings
            .values()
            .filter_map(|binding| binding.allocation_task.as_ref())
            .collect::<Vec<_>>();
        let scope = ConsensusScope::CurrencyAllocation {
            validator_set_version: snapshot.validator_set.version(),
            start: snapshot.state.next_currency_address(),
        };
        let preferred = store
            .bft_local_state(runtime.validator_id(), &scope)?
            .and_then(|state| {
                state
                    .valid_prevote_qc()
                    .and_then(|proof| proof.statement().value().digest())
                    .or_else(|| state.locked_digest())
            });
        if let Some(preferred) = preferred
            && let Some(index) = pending.iter().position(|task| {
                let Some(binding) = snapshot
                    .state
                    .protocol
                    .task_bindings
                    .get(&task.payload().task_id())
                else {
                    return false;
                };
                let Ok(count) = crate::currency_allocation::required_count_for_operations(
                    task.payload().operations(),
                ) else {
                    return false;
                };
                binding.allocation.is_none()
                    && crate::CurrencyAllocation::digest_for(
                        snapshot.validator_set.version(),
                        snapshot.state.next_currency_address(),
                        count,
                        binding.request_digest,
                    ) == preferred
            })
        {
            // Restore the locked candidate before filling the bounded admission window.
            pending.swap(0, index);
        }
        let mut allocation_registered = false;
        for task in pending {
            // Seed one allocation at this frontier; later certified frontiers
            // resume the durable queue. Already allocated business still resumes
            // in full, and direct/fetched candidates retain their original bound.
            if allocation_registered
                && snapshot.state.protocol.task_bindings[&task.payload().task_id()]
                    .allocation
                    .is_none()
                && crate::currency_allocation::required_count_for_operations(
                    task.payload().operations(),
                )
                .map_err(crate::PreparationError::from)?
                    > 0
            {
                continue;
            }
            let verified = task.verify(runtime.authorizers())?;
            let current = store
                .load_shared()?
                .ok_or(NodeRuntimeError::SnapshotMissing)?;
            if !current
                .state
                .protocol
                .task_bindings
                .contains_key(&verified.task_id())
            {
                continue;
            }
            allocation_registered |= self.resume_allocation_task(&verified, current)?;
        }
        Ok(())
    }
    fn resume_allocation_task(
        &self,
        verified: &crate::VerifiedLegalTask,
        current: std::sync::Arc<crate::PersistedNodeState>,
    ) -> Result<bool, NodeRuntimeError> {
        let store = self.full_store()?;
        let runtime = self
            .validator_bft
            .as_ref()
            .ok_or(NodeRuntimeError::ValidatorBftNotConfigured)?;
        let context = self
            .task_submission_context()
            .ok_or(NodeRuntimeError::ValidatorBftNotConfigured)?;
        let allocated = current.state.protocol.task_bindings[&verified.task_id()]
            .allocation
            .is_some();
        let requires_allocation = crate::currency_allocation::required_count_for_operations(
            verified.signed_task().payload().operations(),
        )
        .map_err(crate::PreparationError::from)?
            > 0;
        let scope = if allocated || !requires_allocation {
            ConsensusScope::PreparedTask(verified.task_id())
        } else {
            ConsensusScope::CurrencyAllocation {
                validator_set_version: current.validator_set.version(),
                start: current.state.next_currency_address(),
            }
        };
        match context.submit_verified(verified.signed_task(), verified, current) {
            Ok(crate::LegalTaskSubmissionOutcome::Allocating) => return Ok(true),
            Ok(crate::LegalTaskSubmissionOutcome::AlreadyPending) => {
                if !store
                    .load_shared()?
                    .ok_or(NodeRuntimeError::SnapshotMissing)?
                    .prepared_tasks
                    .contains_key(&verified.task_id())
                {
                    return Ok(requires_allocation && !allocated);
                }
            }
            Ok(_) => {}
            Err(NodeRuntimeError::BftConsensus(
                BftConsensusRuntimeError::PendingFutureMessagesFull(_),
            )) => {
                // The durable queue stays outside the candidate window until
                // the next certified frontier retries it through this same path.
                return Ok(true);
            }
            Err(error @ NodeRuntimeError::Preparation(PreparationError::Persistence(_))) => {
                return Err(error);
            }
            Err(NodeRuntimeError::Preparation(error)) => {
                runtime.consensus().record_rejection(
                    None,
                    scope,
                    BftConsensusRuntimeError::Preparation(error),
                );
                // A changed business state can reject a retired request without
                // authorizing deletion of its original signature or a terminal
                // cancellation. Preserve it for explicit retry or later recovery.
                return Ok(false);
            }
            Err(error) => return Err(error),
        }
        // Clear the durable source only after preparation and consensus
        // registration succeed. Business rejection is not certified cancellation.
        store.finish_allocation_preparation(&verified.task_id())?;
        Ok(false)
    }
}
