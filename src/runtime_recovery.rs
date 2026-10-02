use std::sync::Arc;

use crate::network::StateRecoveryProvider;
use crate::{CertifiedStateRecoveryCheckpoint, NodeRuntime, NodeRuntimeError};

impl NodeRuntime {
    pub fn publish_state_recovery_checkpoint(
        &self,
        checkpoint: CertifiedStateRecoveryCheckpoint,
    ) -> Result<(), NodeRuntimeError> {
        self.full_store()?
            .advance_recovery_checkpoint_floor(&checkpoint)?;
        self.publish_state_recovery_provider(&checkpoint)
    }

    pub(crate) fn publish_state_recovery_provider(
        &self,
        checkpoint: &CertifiedStateRecoveryCheckpoint,
    ) -> Result<(), NodeRuntimeError> {
        let persisted = self
            .full_store()?
            .load()?
            .ok_or(NodeRuntimeError::SnapshotMissing)?;
        let provider = Arc::new(StateRecoveryProvider::new(
            &persisted.state,
            &persisted.validator_set,
            &persisted.validator_registry,
            checkpoint,
        )?);
        *self
            .state_recovery_provider_handle()
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(provider);
        Ok(())
    }
}
