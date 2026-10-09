//! Accepted request bodies without frozen plans carry no old voting or resource rights.
use super::*;

impl TaskHandoff {
    pub(crate) fn insert_request(
        &mut self,
        task: crate::LegalTask,
    ) -> Result<(), PersistenceError> {
        let task_id = task.payload().task_id();
        if let Some(plan) = self.task_context(&task_id) {
            return if plan.source_task == task {
                Ok(())
            } else {
                Err(PersistenceError::InvalidSnapshot)
            };
        }
        if let Some(previous) = self.requests.get(&task_id) {
            return if previous == &task {
                Ok(())
            } else {
                Err(PersistenceError::InvalidSnapshot)
            };
        }
        self.requests.insert(task_id, task);
        self.encoded.take();
        self.verified_proof.take();
        Ok(())
    }

    pub(crate) fn admit_requests(
        &self,
        state: &mut crate::SecondState,
        authorizers: &crate::AuthorizerSet,
    ) -> Result<(), PersistenceError> {
        for task in self.requests.values() {
            let verified = task
                .clone()
                .verify(authorizers)
                .map_err(|_| PersistenceError::InvalidSnapshot)?;
            state
                .authorize_task(&verified)
                .map_err(|_| PersistenceError::InvalidSnapshot)?;
            if let Some(binding) = state.protocol.task_bindings.get(&verified.task_id())
                && binding.outcome.is_terminal()
            {
                if binding.request_digest != verified.request_digest() {
                    return Err(PersistenceError::InvalidSnapshot);
                }
                continue;
            }
            state
                .bind_task(&verified)
                .map_err(|_| PersistenceError::InvalidSnapshot)?;
            let binding = state
                .protocol
                .task_bindings
                .get_mut(&verified.task_id())
                .unwrap();
            if !binding.outcome.is_terminal() {
                if binding
                    .allocation_task
                    .as_ref()
                    .is_some_and(|saved| saved != task)
                {
                    return Err(PersistenceError::InvalidSnapshot);
                }
                binding.allocation_task = Some(task.clone());
            }
        }
        Ok(())
    }

    pub(super) fn covers_requests(
        &self,
        snapshot: &PersistedNodeState,
        admitted: Option<&Self>,
    ) -> Result<(), PersistenceError> {
        for (task_id, task) in &self.requests {
            let binding = snapshot
                .state
                .protocol
                .task_bindings
                .get(task_id)
                .ok_or(PersistenceError::StalePreparedTasks)?;
            if binding.request_digest
                != task
                    .request_digest()
                    .map_err(|_| PersistenceError::InvalidSnapshot)?
            {
                return Err(PersistenceError::InvalidSnapshot);
            }
            let inherited = snapshot
                .state
                .protocol
                .task_handoff
                .as_ref()
                .and_then(|previous| previous.requests.get(task_id));
            let remembered = admitted.and_then(|previous| previous.requests.get(task_id));
            if binding
                .allocation_task
                .as_ref()
                .or(inherited)
                .or(remembered)
                != Some(task)
                && binding.allocation_certificate.is_none()
            {
                return Err(PersistenceError::StalePreparedTasks);
            }
        }
        for (task_id, binding) in &snapshot.state.protocol.task_bindings {
            if admitted.is_none()
                && !binding.outcome.is_terminal()
                && let Some(task) = &binding.allocation_task
                && self.requests.get(task_id) != Some(task)
                && self
                    .task_context(task_id)
                    .is_none_or(|plan| plan.source_task != *task)
            {
                return Err(PersistenceError::StalePreparedTasks);
            }
        }
        if let Some(previous) = &snapshot.state.protocol.task_handoff {
            for (task_id, task) in &previous.requests {
                if !snapshot
                    .state
                    .protocol
                    .task_bindings
                    .get(task_id)
                    .is_some_and(|binding| binding.outcome.is_terminal())
                    && self.requests.get(task_id) != Some(task)
                    && self
                        .task_context(task_id)
                        .is_none_or(|plan| plan.source_task != *task)
                {
                    return Err(PersistenceError::StalePreparedTasks);
                }
            }
        }
        Ok(())
    }
}
