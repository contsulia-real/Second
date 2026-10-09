use super::StateStore;
use crate::{CurrencyAllocation, FinalityCertificate, PersistenceError, VerifiedLegalTask};

impl StateStore {
    pub(crate) fn validate_currency_allocation(
        &self,
        allocation: &CurrencyAllocation,
    ) -> Result<(), PersistenceError> {
        let snapshot = self
            .load_shared()?
            .ok_or(PersistenceError::MissingSnapshot)?;
        if snapshot.validator_set.version() != allocation.validator_set_version
            || snapshot.state.next_currency_address() != allocation.start
        {
            return Err(PersistenceError::StaleState);
        }
        if let Some(binding) = snapshot
            .state
            .protocol
            .task_bindings
            .get(&allocation.task_id())
            && (binding.request_digest
                != allocation
                    .task
                    .request_digest()
                    .map_err(|_| PersistenceError::InvalidSnapshot)?
                || binding.allocation.is_some()
                || binding.outcome.is_terminal())
        {
            return Err(PersistenceError::StaleState);
        }
        Ok(())
    }

    pub(crate) fn queue_currency_allocation(
        &self,
        task: &VerifiedLegalTask,
    ) -> Result<bool, PersistenceError> {
        let snapshot = self.load()?.ok_or(PersistenceError::MissingSnapshot)?;
        let mut state = snapshot.state.clone();
        state
            .bind_task(task)
            .map_err(|_| PersistenceError::StaleState)?;
        let binding = state
            .protocol
            .task_bindings
            .get_mut(&task.task_id())
            .unwrap();
        if binding.outcome.is_terminal() || binding.allocation.is_some() {
            return Ok(false);
        }
        if binding.allocation_task.is_some() {
            return Ok(true);
        }
        binding.allocation_task = Some(task.signed_task().clone());
        self.save_with_prepared(
            &snapshot.state,
            &state,
            &snapshot.validator_set,
            &snapshot.prepared_tasks,
            &snapshot.prepared_tasks,
        )?;
        Ok(true)
    }

    pub fn install_currency_allocation(
        &self,
        allocation: &CurrencyAllocation,
        certificate: &FinalityCertificate,
    ) -> Result<(), PersistenceError> {
        let snapshot = self.load()?.ok_or(PersistenceError::MissingSnapshot)?;
        certificate
            .verify(&snapshot.validator_set)
            .map_err(PersistenceError::CheckpointFinality)?;
        if certificate.statement() != allocation.finality_statement() {
            return Err(PersistenceError::InvalidSnapshot);
        }
        let digest = allocation
            .task
            .request_digest()
            .map_err(|_| PersistenceError::InvalidSnapshot)?;
        if let Some(binding) = snapshot
            .state
            .protocol
            .task_bindings
            .get(&allocation.task_id())
            && binding.request_digest == digest
            && binding.allocation == Some((allocation.start, allocation.count))
        {
            return Ok(());
        }
        self.validate_currency_allocation(allocation)?;
        let mut state = snapshot.state.clone();
        state
            .protocol
            .task_bindings
            .entry(allocation.task_id())
            .or_insert(crate::state::TaskBinding {
                request_digest: digest,
                outcome: crate::state::TaskOutcome::Pending,
                allocation: None,
                allocation_task: None,
                allocation_certificate: None,
            });
        let binding = state
            .protocol
            .task_bindings
            .get_mut(&allocation.task_id())
            .unwrap();
        binding.allocation = Some((allocation.start, allocation.count));
        binding.allocation_task = Some(allocation.task.clone());
        binding.allocation_certificate = Some(certificate.clone());
        state.protocol.next_currency_address = allocation.start + allocation.count;
        self.save_with_prepared(
            &snapshot.state,
            &state,
            &snapshot.validator_set,
            &snapshot.prepared_tasks,
            &snapshot.prepared_tasks,
        )?;
        Ok(())
    }

    pub(crate) fn finish_allocation_preparation(
        &self,
        task_id: &crate::TaskId,
    ) -> Result<(), PersistenceError> {
        let snapshot = self.load()?.ok_or(PersistenceError::MissingSnapshot)?;
        let mut state = snapshot.state.clone();
        let Some(binding) = state.protocol.task_bindings.get_mut(task_id) else {
            return Ok(());
        };
        if binding.allocation_task.take().is_none() {
            return Ok(());
        }
        self.save_with_prepared(
            &snapshot.state,
            &state,
            &snapshot.validator_set,
            &snapshot.prepared_tasks,
            &snapshot.prepared_tasks,
        )?;
        Ok(())
    }
}
