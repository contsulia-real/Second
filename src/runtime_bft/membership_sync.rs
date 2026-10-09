use std::time::Duration;

use super::{ValidatorBftRuntime, ValidatorBftRuntimeError};
use crate::PersistenceError;
use crate::network::{
    NetworkError, PeerRecord, QuicClient, QuicPeer, client_validator_set_transition_proof,
};

const MAX_TRANSITIONS_PER_DIAL: usize = 64;
const MEMBERSHIP_SYNC_TIMEOUT: Duration = Duration::from_secs(5);

#[cfg(test)]
mod tests;

impl ValidatorBftRuntime {
    /// The signed handshake version only triggers a bounded request. Every step
    /// uses the existing proof verifier and atomic store activation from our anchor.
    pub(super) async fn sync_durable_membership(
        &self,
        client: &QuicClient,
        record: &PeerRecord,
        target_version: u64,
    ) -> Result<(), ValidatorBftRuntimeError> {
        let result = tokio::time::timeout(MEMBERSHIP_SYNC_TIMEOUT, async {
            let peer = client
                .connect_expected(record.address(), record.node_id())
                .await?;
            let result = self
                .apply_membership_chain(&peer, client, target_version)
                .await;
            peer.close();
            result
        })
        .await;
        // Partial progress is durable, including its locked signing state.
        self.refresh_authority_for_network().await?;
        self.inner.consensus.wake();
        result.unwrap_or_else(|_| Err(unavailable("membership proof request timed out")))
    }

    async fn apply_membership_chain(
        &self,
        peer: &QuicPeer,
        client: &QuicClient,
        target_version: u64,
    ) -> Result<(), ValidatorBftRuntimeError> {
        for _ in 0..MAX_TRANSITIONS_PER_DIAL {
            let snapshot = self.membership_snapshot().await?;
            if snapshot.validator_set.version() >= target_version {
                return Ok(());
            }
            let Some(proof) =
                client_validator_set_transition_proof(peer, snapshot.validator_set.version())
                    .await?
            else {
                return Err(unavailable("membership proof is unavailable"));
            };
            let store = self.inner.store.clone();
            let validator = self.validator_id();
            let validating = std::sync::Arc::clone(&snapshot);
            let installing_proof = proof.clone();
            let missing_body = tokio::task::spawn_blocking(move || {
                let certified = installing_proof
                    .verify(&validating.validator_set, &validating.validator_registry)
                    .map_err(|_| unavailable("membership proof failed verification"))?;
                if let Err(error) =
                    store.activate_validator_set_transition_for_runtime(&certified, validator)
                {
                    let latest = store
                        .load_shared()?
                        .ok_or(PersistenceError::MissingSnapshot)?;
                    if latest.validator_set.version() <= validating.validator_set.version() {
                        if error == PersistenceError::StalePreparedTasks
                            && certified.transition().handoff_digest.is_some()
                        {
                            return Ok(Some(certified));
                        }
                        return Err(error.into());
                    }
                }
                Ok::<_, ValidatorBftRuntimeError>(None)
            })
            .await
            .map_err(|error| {
                unavailable(&format!("membership install worker failed: {error}"))
            })??;
            if let Some(certified) = missing_body {
                // Proof-only sessions cannot serve private bodies. Pin the same
                // provider on a fresh connection and use the existing identity
                // authorization for this exact certified root.
                let body_peer = client
                    .connect_expected(peer.remote_address(), peer.remote_node_id())
                    .await?;
                let body = crate::network::client_fetch_validator_handoff(
                    &body_peer,
                    validator,
                    self.inner.keys.identity_key(),
                    &proof,
                    &snapshot,
                )
                .await;
                body_peer.close();
                let body = body?;
                let store = self.inner.store.clone();
                let authorizers = self.authorizers().clone();
                let now = self.now();
                tokio::task::spawn_blocking(move || {
                    let certified =
                        certified.with_handoff(crate::persistence::TaskHandoff::decode(&body)?)?;
                    let mut latest = store.load()?.ok_or(PersistenceError::MissingSnapshot)?;
                    if latest.validator_set.version() > snapshot.validator_set.version() {
                        return Ok(());
                    }
                    crate::PreparedTaskBook::from_tasks(store.clone(), latest.prepared_tasks)
                        .and_then(|mut tasks| {
                            tasks.collect_transition_handoff(
                                &mut latest.state,
                                certified.transition(),
                                &authorizers,
                                now,
                            )
                        })
                        .map(|_| ())
                        .map_err(|error| match error {
                            crate::PreparationError::Persistence(error) => error.into(),
                            error => {
                                unavailable(&format!("membership body admission failed: {error:?}"))
                            }
                        })?;
                    store.activate_validator_set_transition_for_runtime(&certified, validator)?;
                    Ok::<_, ValidatorBftRuntimeError>(())
                })
                .await
                .map_err(|error| {
                    unavailable(&format!("membership body worker failed: {error}"))
                })??;
            }
        }
        let snapshot = self.membership_snapshot().await?;
        if snapshot.validator_set.version() >= target_version {
            Ok(())
        } else {
            Err(unavailable("membership proof batch limit reached"))
        }
    }

    async fn membership_snapshot(
        &self,
    ) -> Result<std::sync::Arc<crate::PersistedNodeState>, ValidatorBftRuntimeError> {
        let store = self.inner.store.clone();
        tokio::task::spawn_blocking(move || {
            store
                .load_shared()?
                .ok_or(PersistenceError::MissingSnapshot)
        })
        .await
        .map_err(|error| unavailable(&format!("membership snapshot worker failed: {error}")))?
        .map_err(Into::into)
    }
}

fn unavailable(message: &str) -> ValidatorBftRuntimeError {
    // A missing/bad provider must not permanently blacklist the pinned identity;
    // normal connection maintenance can try another provider or the next batch.
    NetworkError::Transport(message.to_owned()).into()
}

pub(crate) fn load_transition_proof(
    store: Option<&crate::StateStore>,
    current_validator_set_version: u64,
) -> Result<Option<crate::ValidatorSetTransitionProof>, NetworkError> {
    match store {
        Some(store) => {
            let persisted = store
                .load_shared()
                .map_err(|error| NetworkError::PublicStateSource(format!("{error:?}")))?
                .ok_or_else(|| NetworkError::PublicStateSource("snapshot missing".to_owned()))?;
            Ok(persisted
                .validator_transition_proofs
                .get(&current_validator_set_version)
                .cloned())
        }
        None => Ok(None),
    }
}
