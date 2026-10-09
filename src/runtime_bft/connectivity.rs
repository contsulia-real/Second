//! Bounded authenticated validator discovery, independent of public peer upkeep.
use super::{ValidatorBftRuntime, ValidatorBftRuntimeError};
use crate::network::{NetworkError, PeerRecord};
use crate::runtime::ActiveConnectionPermit;
#[cfg(test)]
use crate::runtime::MAX_ACTIVE_CONNECTIONS;
use crate::{NodeRuntime, NodeRuntimeError, ValidatorId};
use std::collections::HashSet;

const VALIDATOR_BFT_DIAL_CONCURRENCY: usize = 4;
#[cfg(test)]
mod tests;
impl NodeRuntime {
    pub(crate) async fn maintain_validator_bft_connections(
        &self,
        bootstrap_records: &[PeerRecord],
    ) -> Result<(), NodeRuntimeError> {
        if self.validator_bft.is_none() {
            return Ok(());
        }
        loop {
            self.maintain_validator_bft_peers(bootstrap_records).await?;
            tokio::time::sleep(crate::runtime::PEER_MAINTENANCE_INTERVAL).await;
        }
    }

    pub fn validator_id(&self) -> Option<ValidatorId> {
        self.validator_bft
            .as_ref()
            .map(ValidatorBftRuntime::validator_id)
    }

    pub fn connected_validator_ids(&self) -> Vec<ValidatorId> {
        self.validator_bft
            .as_ref()
            .map(ValidatorBftRuntime::connected_validator_ids)
            .unwrap_or_default()
    }

    #[cfg(test)]
    pub(crate) async fn dial_validator_bft(
        &self,
        record: &PeerRecord,
    ) -> Result<ValidatorId, NodeRuntimeError> {
        let runtime = self
            .validator_bft
            .as_ref()
            .ok_or(NodeRuntimeError::ValidatorBftNotConfigured)?;
        runtime.refresh_authority_for_network().await?;
        let permit = ActiveConnectionPermit::try_acquire(&self.active_connections).ok_or(
            NodeRuntimeError::ConnectionCapacityReached {
                maximum: MAX_ACTIVE_CONNECTIONS,
            },
        )?;
        let validator = runtime
            .dial(
                record,
                self.transport_identity.clone(),
                self.server.clone(),
                permit,
                self.peer_store.clone(),
            )
            .await?;
        Ok(validator)
    }

    pub(crate) async fn maintain_validator_bft_peers(
        &self,
        bootstrap_records: &[PeerRecord],
    ) -> Result<(), NodeRuntimeError> {
        let Some(runtime) = self.validator_bft.as_ref() else {
            return Ok(());
        };
        runtime.refresh_authority_for_network().await?;
        let target = runtime
            .validator_ids()
            .into_iter()
            .filter(|validator_id| *validator_id != runtime.validator_id())
            .count();
        if runtime.connected_validator_ids().len() >= target {
            return Ok(());
        }

        let connected_nodes = runtime.connected_node_ids();
        let rejected_nodes = runtime.rejected_node_ids();
        let mut seen = HashSet::new();
        let mut candidates = self
            .peer_store
            .validator_candidates_async(runtime.validator_ids(), self.node_id())
            .await?;
        candidates.extend(
            bootstrap_records
                .iter()
                .filter(|record| record.node_id() != self.node_id())
                .cloned(),
        );

        let mut candidates = candidates.into_iter().filter(|record| {
            seen.insert(record.clone())
                && !connected_nodes.contains(&record.node_id())
                && !rejected_nodes.contains(&record.node_id())
        });
        let mut dials = tokio::task::JoinSet::new();
        loop {
            while dials.len() < VALIDATOR_BFT_DIAL_CONCURRENCY
                && runtime.connected_validator_ids().len() < target
            {
                let Some(record) = candidates.next() else {
                    break;
                };
                let Some(permit) = ActiveConnectionPermit::try_acquire(&self.active_connections)
                else {
                    break;
                };
                let runtime = runtime.clone();
                let identity = self.transport_identity.clone();
                let server = self.server.clone();
                let peer_store = self.peer_store.clone();
                let started = std::time::Instant::now();
                dials.spawn(async move {
                    let result = runtime
                        .dial(&record, identity, server, permit, peer_store)
                        .await;
                    (record, started.elapsed(), result)
                });
            }
            let Some(result) = dials.join_next().await else {
                break;
            };
            if let Ok((record, elapsed, result)) = result {
                match result {
                    Ok(_) => {}
                    Err(error) => {
                        if matches!(
                            error,
                            ValidatorBftRuntimeError::Network(NetworkError::BftUnauthorized)
                        ) {
                            runtime.reject_node(record.node_id());
                        }
                        runtime.consensus().record_connection_failure(
                            record.node_id(),
                            record.address(),
                            elapsed,
                            error,
                        );
                    }
                }
            }
        }
        Ok(())
    }
}
