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
            .load_shared()?
            .ok_or(NodeRuntimeError::SnapshotMissing)?;
        if !persisted
            .recovery_checkpoint_proof
            .as_ref()
            .is_some_and(|proof| proof.checkpoint() == checkpoint.checkpoint())
        {
            return Err(crate::PersistenceError::RecoveryCheckpointDoesNotMatchState.into());
        }
        if self
            .state_recovery_provider_handle()
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .is_some_and(|provider| {
                provider.checkpoint_digest() == checkpoint.checkpoint().digest()
            })
        {
            return Ok(());
        }
        let provider = Arc::new(StateRecoveryProvider::new(&persisted, checkpoint)?);
        *self
            .state_recovery_provider_handle()
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(provider);
        Ok(())
    }
}
