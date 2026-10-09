//! Restore certified prepared-task completion in the existing bounded relay cache.
use super::*;

impl ValidatorConsensusRuntime {
    pub(crate) fn restore_completed_tasks(
        &self,
        snapshot: &crate::PersistedNodeState,
        relay_interval: Duration,
    ) -> Result<(), BftConsensusRuntimeError> {
        self.restore_completed_tasks_for(
            snapshot,
            relay_interval,
            snapshot.task_receipts.keys(),
            false,
        )
    }

    pub(crate) fn restore_completed_tasks_for<'a>(
        &self,
        snapshot: &crate::PersistedNodeState,
        relay_interval: Duration,
        tasks: impl Iterator<Item = &'a TaskId>,
        emit_completion: bool,
    ) -> Result<(), BftConsensusRuntimeError> {
        let mut coordinator = self
            .coordinator
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for task_id in tasks {
            let Some(receipt) = snapshot.task_receipts.get(task_id) else {
                continue;
            };
            let plan = receipt.plan();
            let validators = (snapshot.validator_set.version() == plan.validator_set_version)
                .then_some(&snapshot.validator_set)
                .or_else(|| {
                    snapshot
                        .retained_validator_sets
                        .get(&plan.validator_set_version)
                })
                .ok_or(BftConsensusRuntimeError::InvalidPreparedTaskSource)?;
            let certificate = receipt
                .certificate()
                .map_err(BftConsensusRuntimeError::Persistence)?;
            coordinator.restore_prepared_completion(
                task_id,
                validators,
                certificate,
                relay_interval,
                emit_completion,
            );
        }
        Ok(())
    }
    // The caller has verified and atomically persisted this terminal certificate.
    pub(crate) fn restore_prepared_terminal(
        &self,
        task_id: &TaskId,
        validators: &ValidatorSet,
        certificate: FinalityCertificate,
        relay_interval: Duration,
    ) {
        self.coordinator
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .restore_prepared_completion(task_id, validators, certificate, relay_interval, true);
    }
}

impl BftConsensusCoordinator {
    fn restore_prepared_completion(
        &mut self,
        task_id: &TaskId,
        validators: &ValidatorSet,
        certificate: FinalityCertificate,
        relay_interval: Duration,
        emit_completion: bool,
    ) {
        let scope = ConsensusScope::PreparedTask(task_id.clone());
        let already_emitted = self
            .sessions
            .remove(&scope)
            .is_some_and(|session| session.certified_emitted);
        self.pending_unregistered.remove(&scope);
        if self
            .recent_completed
            .iter()
            .any(|completed| completed.subject.scope() == &scope)
        {
            return;
        }
        if emit_completion && !already_emitted {
            self.record_event(BftConsensusEvent::CertifiedPreparedTask {
                task_id: task_id.clone(),
                certificate: certificate.clone(),
            });
        }
        self.remember_completed(CompletedConsensusScope {
            subject: BftProposalSubject::new(
                validators.version(),
                scope,
                certificate.statement().subject_digest(),
            ),
            validator_set: validators.clone(),
            certificate,
            recovery_source: None,
            allocation_source: None,
            transition_source: None,
            relay_interval,
            next_relay_at: Instant::now(),
        });
    }
}
