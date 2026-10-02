use std::time::Duration;

use crate::network::{
    QuicPeer, client_public_currency_checkpoint_proof, client_public_currency_delta,
    client_sync_certified_public_currency_view_from_checkpoint,
    client_validator_set_transition_proof,
};
use crate::{
    NodeRuntime, NodeRuntimeError, ValidatorRegistry, ValidatorSet, ValidatorSetTransitionProof,
};

const PUBLIC_STATE_SYNC_INTERVAL: Duration = Duration::from_secs(15);
const PUBLIC_SYNC_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const PUBLIC_SYNC_FULL_VIEW_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_VALIDATOR_TRANSITIONS_PER_SYNC: usize = 64;

impl NodeRuntime {
    pub async fn sync_public_state_once(&self) -> Result<bool, NodeRuntimeError> {
        let Some(store) = self.public_state_store() else {
            return Ok(false);
        };
        let peers = self.active_public_peers();
        if peers.is_empty() {
            return Err(NodeRuntimeError::NoActivePeers);
        }

        let mut candidates = Vec::new();
        for peer in peers {
            let Ok(Ok(Some(proof))) = tokio::time::timeout(
                PUBLIC_SYNC_REQUEST_TIMEOUT,
                client_public_currency_checkpoint_proof(&peer),
            )
            .await
            else {
                continue;
            };
            candidates.push((
                proof.validator_set_version(),
                proof.checkpoint().epoch(),
                peer.remote_node_id(),
                peer,
                proof,
            ));
        }
        candidates.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then_with(|| right.1.cmp(&left.1))
                .then_with(|| left.2.cmp(&right.2))
        });

        let mut advanced_trust = false;
        for (remote_set_version, _, _, peer, proof) in candidates {
            let local = store.load()?.ok_or(NodeRuntimeError::SnapshotMissing)?;
            if remote_set_version < local.validator_set.version() {
                continue;
            }

            if remote_set_version > local.validator_set.version() {
                let progress = collect_transition_progress(
                    &peer,
                    &local.validator_set,
                    &local.validator_registry,
                    remote_set_version,
                )
                .await?;
                if progress.is_empty() {
                    continue;
                }
                let reached_target = progress
                    .last()
                    .is_some_and(|item| item.next_version == remote_set_version);
                for item in progress {
                    store.activate_validator_set_transition(&item.proof)?;
                    advanced_trust = true;
                }
                if !reached_target {
                    return Ok(advanced_trust);
                }
            }

            let current = store.load()?.ok_or(NodeRuntimeError::SnapshotMissing)?;
            if current.validator_set.version() != remote_set_version {
                continue;
            }
            let Ok(checkpoint) = proof.verify_checkpoint(&current.validator_set) else {
                continue;
            };

            if let Some(local_proof) = current.checkpoint_proof.as_ref() {
                let local_epoch = local_proof.checkpoint().epoch();
                let remote_epoch = checkpoint.checkpoint().epoch();
                if remote_epoch < local_epoch {
                    continue;
                }
                if remote_epoch == local_epoch {
                    if local_proof.checkpoint().digest() == checkpoint.checkpoint().digest() {
                        return Ok(advanced_trust);
                    }
                    continue;
                }
            }

            if let (Some(local_view), Some(local_proof)) =
                (current.view.as_ref(), current.checkpoint_proof.as_ref())
            {
                let base_epoch = local_proof.checkpoint().epoch();
                let base_digest = local_view.summary.state_digest;
                let delta_request = client_public_currency_delta(&peer, base_epoch, base_digest);
                if let Ok(Ok(Some(delta))) =
                    tokio::time::timeout(PUBLIC_SYNC_REQUEST_TIMEOUT, delta_request).await
                    && delta.to_epoch() == checkpoint.checkpoint().epoch()
                    && delta.summary() == checkpoint.checkpoint().summary()
                    && let Ok(view) = delta.apply(base_epoch, local_view)
                    && checkpoint
                        .verify_view(&view, &current.validator_set)
                        .is_ok()
                {
                    store.install_certified_view(view, &checkpoint)?;
                    return Ok(true);
                }
            }

            let full_sync = client_sync_certified_public_currency_view_from_checkpoint(
                &peer,
                checkpoint,
                &current.validator_set,
            );
            if let Ok(Ok(synced)) =
                tokio::time::timeout(PUBLIC_SYNC_FULL_VIEW_TIMEOUT, full_sync).await
            {
                store.install_certified_view(synced.view, &synced.checkpoint)?;
                return Ok(true);
            }
        }

        if advanced_trust {
            Ok(true)
        } else {
            Err(NodeRuntimeError::NoCertifiedPublicPeer)
        }
    }

    pub(crate) async fn run_public_state_sync(&self) -> Result<(), NodeRuntimeError> {
        if self.public_state_store().is_none() {
            return std::future::pending::<Result<(), NodeRuntimeError>>().await;
        }

        let mut interval = tokio::time::interval(PUBLIC_STATE_SYNC_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            match self.sync_public_state_once().await {
                Ok(_)
                | Err(NodeRuntimeError::NoActivePeers | NodeRuntimeError::NoCertifiedPublicPeer) => {
                }
                Err(error) => return Err(error),
            }
        }
    }
}

struct TransitionProgress {
    proof: ValidatorSetTransitionProof,
    next_version: u64,
}

async fn collect_transition_progress(
    peer: &QuicPeer,
    current_set: &ValidatorSet,
    current_registry: &ValidatorRegistry,
    target_version: u64,
) -> Result<Vec<TransitionProgress>, NodeRuntimeError> {
    let mut validator_set = current_set.clone();
    let mut validator_registry = current_registry.clone();
    let mut progress = Vec::new();

    while validator_set.version() < target_version
        && progress.len() < MAX_VALIDATOR_TRANSITIONS_PER_SYNC
    {
        let current_version = validator_set.version();
        let request = client_validator_set_transition_proof(peer, current_version);
        let Ok(Ok(Some(proof))) = tokio::time::timeout(PUBLIC_SYNC_REQUEST_TIMEOUT, request).await
        else {
            break;
        };
        let Ok(certified) = proof.verify(&validator_set, &validator_registry) else {
            break;
        };
        let Ok(next_set) = certified.activate(&mut validator_registry) else {
            break;
        };
        let next_version = next_set.version();
        progress.push(TransitionProgress {
            proof,
            next_version,
        });
        validator_set = next_set;
    }

    Ok(progress)
}
