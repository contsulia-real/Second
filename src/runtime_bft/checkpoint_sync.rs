use super::{ValidatorBftRuntime, ValidatorBftRuntimeError, ValidatorBftSendFailure};
use crate::{BftNetworkMessage, ConsensusScope, ValidatorId};

#[cfg(test)]
mod tests;

impl ValidatorBftRuntime {
    /// A locked/restarted member may have missed finality and cannot request it
    /// by voting. Send durable checkpoint evidence on a fresh exact-set dial.
    pub(super) async fn sync_checkpoints(
        &self,
        recipient: ValidatorId,
    ) -> Result<(), ValidatorBftRuntimeError> {
        let runtime = self.clone();
        tokio::task::spawn_blocking(move || runtime.replay_checkpoints(recipient))
            .await
            .map_err(|error| {
                crate::NetworkError::Transport(format!("checkpoint replay worker failed: {error}"))
                    .into()
            })
    }

    fn replay_checkpoints(&self, recipient: ValidatorId) {
        let Ok(Some(snapshot)) = self.inner.store.load_shared() else {
            return;
        };
        if !snapshot.validator_set.contains(self.validator_id())
            || !snapshot.validator_set.contains(recipient)
        {
            return;
        }
        let version = snapshot.validator_set.version();
        if let Some(proof) = snapshot
            .public_checkpoint_proof
            .as_ref()
            .or(snapshot.public_checkpoint_baseline.as_ref())
            .filter(|proof| proof.validator_set_version() == version)
            && let Ok(bytes) = proof.encode_bytes()
        {
            self.send_initial_evidence(
                recipient,
                BftNetworkMessage::PublicCheckpointSource {
                    validator_set_version: version,
                    scope: ConsensusScope::PublicCheckpoint {
                        validator_set_version: version,
                        epoch: proof.checkpoint().epoch(),
                    },
                    bytes,
                },
            );
        }
        if let Some(proof) = snapshot
            .recovery_checkpoint_proof
            .as_ref()
            .filter(|proof| proof.checkpoint().validator_set_version() == version)
            && let Ok(bytes) = proof.encode_bytes()
        {
            self.send_initial_evidence(
                recipient,
                BftNetworkMessage::StateRecoveryCheckpointSource {
                    validator_set_version: version,
                    scope: ConsensusScope::StateRecoveryCheckpoint {
                        validator_set_version: version,
                        serial: proof.checkpoint().serial(),
                    },
                    bytes,
                },
            );
        }
        self.replay_pending_sources(recipient, &snapshot);
    }

    pub(super) fn send_initial_evidence(&self, recipient: ValidatorId, message: BftNetworkMessage) {
        if let Err(error) = self.send_direct(recipient, &message) {
            self.inner.consensus.record_send_failures(
                message.scope().clone(),
                vec![ValidatorBftSendFailure {
                    validator_id: recipient,
                    error,
                }],
            );
        }
    }
}
