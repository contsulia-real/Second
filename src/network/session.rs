use crate::{
    CertifiedPublicCurrencyCheckpoint, CurrencyAddress, PublicCurrencyCheckpointProof,
    PublicCurrencyDelta, PublicCurrencyState, PublicCurrencyView, SecondState, ValidatorSet,
    ValidatorSetTransitionProof,
};

use super::quic::{QuicPeer, QuicRequestStream};

const MAX_PUBLIC_SYNC_STATE_BYTES: usize = 64 * 1024 * 1024;

pub(crate) type RuntimeNetworkSnapshot = std::sync::Arc<crate::PersistedNodeState>;

pub(crate) struct RuntimePublicSnapshot {
    pub(crate) view: Option<std::sync::Arc<PublicCurrencyView>>,
    pub(crate) checkpoint_proof: Option<PublicCurrencyCheckpointProof>,
    pub(crate) latest_delta: Option<PublicCurrencyDelta>,
}

type RuntimeNetworkSnapshotLoader =
    std::sync::Arc<dyn Fn() -> Result<RuntimeNetworkSnapshot, NetworkError> + Send + Sync>;
type RuntimePublicSnapshotLoader =
    std::sync::Arc<dyn Fn(bool) -> Result<RuntimePublicSnapshot, NetworkError> + Send + Sync>;
type ValidatorTransitionProofLoader = std::sync::Arc<
    dyn Fn(u64) -> Result<Option<ValidatorSetTransitionProof>, NetworkError> + Send + Sync,
>;

#[derive(Clone)]
pub(crate) struct PublicNetworkServices {
    peer_store: PeerStore,
    local_node_id: NodeId,
    local_peer_record: Option<std::sync::Arc<PeerRecord>>,
    state_recovery_provider: StateRecoveryProviderHandle,
    public_snapshot_loader: RuntimePublicSnapshotLoader,
    transition_proof_loader: ValidatorTransitionProofLoader,
    recovery_snapshot_loader: Option<RuntimeNetworkSnapshotLoader>,
}

impl PublicNetworkServices {
    pub(crate) const fn new(
        peer_store: PeerStore,
        local_node_id: NodeId,
        local_peer_record: Option<std::sync::Arc<PeerRecord>>,
        state_recovery_provider: StateRecoveryProviderHandle,
        public_snapshot_loader: RuntimePublicSnapshotLoader,
        transition_proof_loader: ValidatorTransitionProofLoader,
        recovery_snapshot_loader: Option<RuntimeNetworkSnapshotLoader>,
    ) -> Self {
        Self {
            peer_store,
            local_node_id,
            local_peer_record,
            state_recovery_provider,
            public_snapshot_loader,
            transition_proof_loader,
            recovery_snapshot_loader,
        }
    }
}

use super::{
    MAX_PUBLIC_CURRENCY_PAGE, NetworkError, NetworkMessage, NodeId, PeerRecord, PeerStore,
    RemoteCertifiedPublicCurrencyView, RemotePublicCurrencyPage, RemotePublicCurrencySummary,
    RemotePublicCurrencyView, StateRecoveryProviderHandle, state_recovery_response,
    validate_peer_limit, validate_public_currency_limit,
};

pub async fn client_ping(peer: &QuicPeer, nonce: u64) -> Result<NodeId, NetworkError> {
    match peer.exchange(&NetworkMessage::Ping { nonce }).await? {
        NetworkMessage::Pong {
            nonce: response_nonce,
        } if response_nonce == nonce => Ok(peer.remote_node_id()),
        NetworkMessage::Pong {
            nonce: response_nonce,
        } => Err(NetworkError::NonceMismatch {
            expected: nonce,
            actual: response_nonce,
        }),
        _ => Err(NetworkError::UnexpectedMessage),
    }
}

pub async fn client_peer_records(
    peer: &QuicPeer,
    limit: u16,
) -> Result<Vec<PeerRecord>, NetworkError> {
    validate_peer_limit(limit)?;
    match peer.exchange(&NetworkMessage::GetPeers { limit }).await? {
        NetworkMessage::Peers { records } => {
            if records.len() > usize::from(limit) {
                return Err(NetworkError::TooManyPeerRecords {
                    announced: records.len(),
                    maximum: usize::from(limit),
                });
            }
            Ok(records)
        }
        _ => Err(NetworkError::UnexpectedMessage),
    }
}

pub async fn serve_ping_session(peer: &QuicPeer) -> Result<NodeId, NetworkError> {
    let request = peer
        .accept_request()
        .await?
        .ok_or_else(|| NetworkError::Transport("peer closed before ping request".to_owned()))?;

    let response = match request.message() {
        NetworkMessage::Ping { nonce } => NetworkMessage::Pong { nonce: *nonce },
        _ => return Err(NetworkError::UnexpectedMessage),
    };

    request.respond(&response).await?;
    Ok(peer.remote_node_id())
}

pub async fn client_public_currency_page(
    peer: &QuicPeer,
    start: CurrencyAddress,
    limit: u16,
) -> Result<RemotePublicCurrencyPage, NetworkError> {
    validate_public_currency_limit(limit)?;
    let (states, next_start) = request_public_currency_page(peer, start, limit).await?;

    Ok(RemotePublicCurrencyPage {
        remote_node_id: peer.remote_node_id(),
        states,
        next_start,
    })
}

pub async fn client_public_currency_summary(
    peer: &QuicPeer,
) -> Result<RemotePublicCurrencySummary, NetworkError> {
    let summary = request_public_currency_summary(peer).await?;

    Ok(RemotePublicCurrencySummary {
        remote_node_id: peer.remote_node_id(),
        summary,
    })
}

pub async fn client_public_currency_checkpoint_proof(
    peer: &QuicPeer,
) -> Result<Option<PublicCurrencyCheckpointProof>, NetworkError> {
    request_public_currency_checkpoint_proof(peer).await
}

pub async fn client_public_currency_delta(
    peer: &QuicPeer,
    from_epoch: u64,
    from_state_digest: [u8; 32],
) -> Result<Option<PublicCurrencyDelta>, NetworkError> {
    match peer
        .exchange(&NetworkMessage::GetPublicCurrencyDelta {
            from_epoch,
            from_state_digest,
        })
        .await?
    {
        NetworkMessage::PublicCurrencyDelta { delta }
            if delta.from_epoch() == from_epoch
                && delta.from_state_digest() == from_state_digest =>
        {
            Ok(Some(delta))
        }
        NetworkMessage::NoPublicCurrencyDelta => Ok(None),
        NetworkMessage::PublicCurrencyDelta { .. } => Err(NetworkError::InvalidPublicCurrencyDelta),
        _ => Err(NetworkError::UnexpectedMessage),
    }
}

pub async fn client_validator_set_transition_proof(
    peer: &QuicPeer,
    current_validator_set_version: u64,
) -> Result<Option<ValidatorSetTransitionProof>, NetworkError> {
    match peer
        .exchange(&NetworkMessage::GetValidatorSetTransitionProof {
            current_validator_set_version,
        })
        .await?
    {
        NetworkMessage::ValidatorSetTransitionProof { proof }
            if proof.source().next_validator_set().version()
                == current_validator_set_version.saturating_add(1) =>
        {
            Ok(Some(proof))
        }
        NetworkMessage::NoValidatorSetTransitionProof => Ok(None),
        NetworkMessage::ValidatorSetTransitionProof { .. } => {
            Err(NetworkError::InvalidValidatorTransitionProof)
        }
        _ => Err(NetworkError::UnexpectedMessage),
    }
}

pub async fn client_sync_certified_public_currency_view(
    peer: &QuicPeer,
    validator_set: &ValidatorSet,
    minimum_checkpoint_epoch: u64,
) -> Result<RemoteCertifiedPublicCurrencyView, NetworkError> {
    let proof = request_public_currency_checkpoint_proof(peer)
        .await?
        .ok_or(NetworkError::MissingPublicCurrencyCheckpoint)?;

    let actual_epoch = proof.checkpoint().epoch();
    if actual_epoch < minimum_checkpoint_epoch {
        return Err(NetworkError::StalePublicCurrencyCheckpoint {
            minimum_epoch: minimum_checkpoint_epoch,
            actual_epoch,
        });
    }

    let checkpoint = proof
        .verify_checkpoint(validator_set)
        .map_err(NetworkError::PublicCheckpoint)?;

    client_sync_certified_public_currency_view_from_checkpoint(peer, checkpoint, validator_set)
        .await
}

pub(crate) async fn client_sync_certified_public_currency_view_from_checkpoint(
    peer: &QuicPeer,
    checkpoint: CertifiedPublicCurrencyCheckpoint,
    validator_set: &ValidatorSet,
) -> Result<RemoteCertifiedPublicCurrencyView, NetworkError> {
    let summary = checkpoint.checkpoint().summary().clone();
    let view = sync_public_currency_view_for_summary(peer, summary).await?;
    checkpoint
        .verify_view(&view, validator_set)
        .map_err(NetworkError::PublicCheckpoint)?;

    Ok(RemoteCertifiedPublicCurrencyView {
        remote_node_id: peer.remote_node_id(),
        view,
        checkpoint,
    })
}

pub async fn client_sync_public_currency_view(
    peer: &QuicPeer,
) -> Result<RemotePublicCurrencyView, NetworkError> {
    let summary = request_public_currency_summary(peer).await?;
    let view = sync_public_currency_view_for_summary(peer, summary).await?;

    Ok(RemotePublicCurrencyView {
        remote_node_id: peer.remote_node_id(),
        view,
    })
}

async fn sync_public_currency_view_for_summary(
    peer: &QuicPeer,
    summary: crate::PublicCurrencySummary,
) -> Result<PublicCurrencyView, NetworkError> {
    let maximum = max_public_sync_states();
    if summary.current_supply > maximum {
        return Err(NetworkError::PublicCurrencySyncTooLarge {
            announced: summary.current_supply,
            maximum,
        });
    }

    let requested = usize::try_from(summary.current_supply).map_err(|_| {
        NetworkError::PublicCurrencySyncTooLarge {
            announced: summary.current_supply,
            maximum,
        }
    })?;
    let mut states = Vec::new();
    states.try_reserve_exact(requested).map_err(|_| {
        NetworkError::PublicCurrencySyncAllocationFailed {
            requested: summary.current_supply,
        }
    })?;

    if summary.current_supply > 0 {
        let mut start = CurrencyAddress::new(0);

        loop {
            let remaining = summary.current_supply.saturating_sub(states.len() as u64);
            if remaining == 0 {
                return Err(NetworkError::InvalidPublicCurrencyPage);
            }

            let limit = remaining.min(u64::from(MAX_PUBLIC_CURRENCY_PAGE)) as u16;
            let (page_states, next_start) =
                request_public_currency_page(peer, start, limit).await?;

            validate_synced_page(
                start,
                limit,
                summary.next_currency_address,
                &page_states,
                next_start,
            )?;

            states.extend(page_states);
            let actual = states.len() as u64;
            if actual > summary.current_supply {
                return Err(NetworkError::SynchronizedCurrencyCountExceeded {
                    claimed: summary.current_supply,
                    actual,
                });
            }

            match next_start {
                Some(next) => {
                    if actual >= summary.current_supply {
                        return Err(NetworkError::InvalidPublicCurrencyPage);
                    }
                    start = next;
                }
                None => break,
            }
        }
    }

    PublicCurrencyView::new(summary, states).map_err(NetworkError::PublicState)
}

pub async fn serve_public_currency_connection(
    peer: &QuicPeer,
    state: &SecondState,
    checkpoint_proof: Option<&PublicCurrencyCheckpointProof>,
) -> Result<NodeId, NetworkError> {
    let view = PublicCurrencyView::new(
        state.public_currency_summary(),
        state.public_currency_states(),
    )
    .map_err(NetworkError::PublicState)?;
    if checkpoint_proof.is_some_and(|proof| proof.checkpoint().summary() != &view.summary) {
        return Err(NetworkError::CheckpointDoesNotMatchServedState);
    }
    serve_public_connection(peer, Some((&view, checkpoint_proof)), None, None).await
}

pub(crate) async fn serve_public_network_connection(
    peer: &QuicPeer,
    services: PublicNetworkServices,
) -> Result<NodeId, NetworkError> {
    serve_public_connection(peer, None, Some(services), None).await
}

pub(crate) async fn serve_public_network_connection_from_request(
    peer: &QuicPeer,
    services: PublicNetworkServices,
    first_request: QuicRequestStream,
) -> Result<NodeId, NetworkError> {
    serve_public_connection(peer, None, Some(services), Some(first_request)).await
}

async fn serve_public_connection(
    peer: &QuicPeer,
    static_public_state: Option<(&PublicCurrencyView, Option<&PublicCurrencyCheckpointProof>)>,
    services: Option<PublicNetworkServices>,
    mut first_request: Option<QuicRequestStream>,
) -> Result<NodeId, NetworkError> {
    loop {
        let request = match first_request.take() {
            Some(request) => request,
            None => {
                let Some(request) = peer.accept_request().await? else {
                    return Ok(peer.remote_node_id());
                };
                request
            }
        };

        let response = if let Some(services) = &services
            && !matches!(request.message(), NetworkMessage::Ping { .. })
        {
            let services = services.clone();
            let message = request.message().clone();
            let peer = peer.clone();
            tokio::task::spawn_blocking(move || {
                public_connection_response(&peer, &message, None, Some(&services))
            })
            .await
            .map_err(|error| {
                NetworkError::Transport(format!("public response worker failed: {error}"))
            })??
        } else {
            public_connection_response(
                peer,
                request.message(),
                static_public_state,
                services.as_ref(),
            )?
        };

        request.respond(&response).await?;
    }
}

fn public_connection_response(
    peer: &QuicPeer,
    message: &NetworkMessage,
    static_public_state: Option<(&PublicCurrencyView, Option<&PublicCurrencyCheckpointProof>)>,
    services: Option<&PublicNetworkServices>,
) -> Result<NetworkMessage, NetworkError> {
    if matches!(message, NetworkMessage::AccountQuery { .. }) {
        let snapshot = services
            .and_then(|services| services.recovery_snapshot_loader.as_ref())
            .map(|loader| loader())
            .transpose()?;
        return super::account_query::response(peer, message, snapshot.as_deref());
    }
    let recovery_response = services.and_then(|services| {
        services
            .recovery_snapshot_loader
            .as_ref()
            .and_then(|loader| {
                state_recovery_response(
                    peer,
                    message,
                    &services.state_recovery_provider,
                    loader.as_ref(),
                )
            })
    });
    let response = match recovery_response {
        Some(response) => response?,
        None if matches!(
            message,
            NetworkMessage::GetValidatorSetTransitionProof { .. }
        ) =>
        {
            transition_proof_response(message, services)?
        }
        None if is_public_currency_request(message) => {
            public_currency_response(message, static_public_state, services)?
        }
        None => match message {
            NetworkMessage::Ping { nonce } => NetworkMessage::Pong { nonce: *nonce },
            NetworkMessage::GetPeers { limit } => {
                validate_peer_limit(*limit)?;
                let services = services.ok_or(NetworkError::UnexpectedMessage)?;
                let mut records = Vec::with_capacity(usize::from(*limit));
                if let Some(record) = services.local_peer_record.as_ref() {
                    records.push((**record).clone());
                }
                let remaining = limit.saturating_sub(records.len() as u16);
                if remaining > 0 {
                    records.extend(
                        services
                            .peer_store
                            .recent(remaining, &[services.local_node_id, peer.remote_node_id()]),
                    );
                }
                NetworkMessage::Peers { records }
            }
            _ => return Err(NetworkError::UnexpectedMessage),
        },
    };

    Ok(response)
}

fn is_public_currency_request(message: &NetworkMessage) -> bool {
    matches!(
        message,
        NetworkMessage::GetPublicCurrencies { .. }
            | NetworkMessage::GetPublicCurrencySummary
            | NetworkMessage::GetPublicCurrencyCheckpoint
            | NetworkMessage::GetPublicCurrencyDelta { .. }
    )
}

fn public_currency_response(
    message: &NetworkMessage,
    static_public_state: Option<(&PublicCurrencyView, Option<&PublicCurrencyCheckpointProof>)>,
    services: Option<&PublicNetworkServices>,
) -> Result<NetworkMessage, NetworkError> {
    if let Some(services) = services {
        let needs_view = matches!(
            message,
            NetworkMessage::GetPublicCurrencies { .. } | NetworkMessage::GetPublicCurrencySummary
        );
        let snapshot = load_runtime_public_snapshot(services, needs_view)?;
        if let NetworkMessage::GetPublicCurrencyCheckpoint = message {
            return Ok(snapshot
                .checkpoint_proof
                .map(|proof| NetworkMessage::PublicCurrencyCheckpointProof { proof })
                .unwrap_or(NetworkMessage::NoPublicCurrencyCheckpoint));
        }
        if let NetworkMessage::GetPublicCurrencyDelta {
            from_epoch,
            from_state_digest,
        } = message
        {
            return Ok(snapshot
                .latest_delta
                .filter(|delta| {
                    delta.from_epoch() == *from_epoch
                        && delta.from_state_digest() == *from_state_digest
                })
                .map(|delta| NetworkMessage::PublicCurrencyDelta { delta })
                .unwrap_or(NetworkMessage::NoPublicCurrencyDelta));
        }
        return public_currency_response_for_snapshot(
            message,
            snapshot
                .view
                .as_deref()
                .ok_or(NetworkError::UnexpectedMessage)?,
            snapshot.checkpoint_proof.as_ref(),
            snapshot.latest_delta.as_ref(),
        );
    }

    let (view, checkpoint_proof) = static_public_state.ok_or(NetworkError::UnexpectedMessage)?;
    public_currency_response_for_snapshot(message, view, checkpoint_proof, None)
}

fn public_currency_response_for_snapshot(
    message: &NetworkMessage,
    view: &PublicCurrencyView,
    checkpoint_proof: Option<&PublicCurrencyCheckpointProof>,
    latest_delta: Option<&PublicCurrencyDelta>,
) -> Result<NetworkMessage, NetworkError> {
    match message {
        NetworkMessage::GetPublicCurrencies { start, limit } => {
            public_currency_page_response(view, *start, *limit)
        }
        NetworkMessage::GetPublicCurrencySummary => Ok(NetworkMessage::PublicCurrencySummary {
            summary: view.summary.clone(),
        }),
        NetworkMessage::GetPublicCurrencyCheckpoint => Ok(checkpoint_proof
            .cloned()
            .map(|proof| NetworkMessage::PublicCurrencyCheckpointProof { proof })
            .unwrap_or(NetworkMessage::NoPublicCurrencyCheckpoint)),
        NetworkMessage::GetPublicCurrencyDelta {
            from_epoch,
            from_state_digest,
        } => Ok(latest_delta
            .filter(|delta| {
                delta.from_epoch() == *from_epoch && delta.from_state_digest() == *from_state_digest
            })
            .cloned()
            .map(|delta| NetworkMessage::PublicCurrencyDelta { delta })
            .unwrap_or(NetworkMessage::NoPublicCurrencyDelta)),
        _ => Err(NetworkError::UnexpectedMessage),
    }
}

fn transition_proof_response(
    message: &NetworkMessage,
    services: Option<&PublicNetworkServices>,
) -> Result<NetworkMessage, NetworkError> {
    let services = services.ok_or(NetworkError::UnexpectedMessage)?;
    transition_proof_response_from_loader(message, &services.transition_proof_loader)
}

fn transition_proof_response_from_loader(
    message: &NetworkMessage,
    loader: &ValidatorTransitionProofLoader,
) -> Result<NetworkMessage, NetworkError> {
    let NetworkMessage::GetValidatorSetTransitionProof {
        current_validator_set_version,
    } = message
    else {
        return Err(NetworkError::UnexpectedMessage);
    };
    Ok(loader(*current_validator_set_version)?
        .map(|proof| NetworkMessage::ValidatorSetTransitionProof { proof })
        .unwrap_or(NetworkMessage::NoValidatorSetTransitionProof))
}

/// A short evidence session does not join peer discovery or replace an existing
/// regular connection. Its owner holds the normal global connection permit.
pub(crate) async fn serve_transition_proof_requests(
    peer: &QuicPeer,
    first_request: QuicRequestStream,
    loader: ValidatorTransitionProofLoader,
) -> Result<(), NetworkError> {
    let mut request = first_request;
    for _ in 0..64 {
        let message = request.message().clone();
        let loader = loader.clone();
        let response = tokio::task::spawn_blocking(move || {
            transition_proof_response_from_loader(&message, &loader)
        })
        .await
        .map_err(|error| {
            NetworkError::Transport(format!("membership response worker failed: {error}"))
        })??;
        request.respond(&response).await?;
        let Some(next) = peer.accept_request().await? else {
            return Ok(());
        };
        request = next;
    }
    peer.close_with_reason(b"membership proof batch limit");
    Ok(())
}

fn load_runtime_public_snapshot(
    services: &PublicNetworkServices,
    needs_view: bool,
) -> Result<RuntimePublicSnapshot, NetworkError> {
    let snapshot = (services.public_snapshot_loader)(needs_view)?;
    if snapshot.checkpoint_proof.as_ref().is_some_and(|proof| {
        snapshot
            .view
            .as_ref()
            .is_some_and(|view| proof.checkpoint().summary() != &view.summary)
    }) {
        return Err(NetworkError::CheckpointDoesNotMatchServedState);
    }
    Ok(snapshot)
}

async fn request_public_currency_page(
    peer: &QuicPeer,
    start: CurrencyAddress,
    limit: u16,
) -> Result<(Vec<crate::PublicCurrencyState>, Option<CurrencyAddress>), NetworkError> {
    validate_public_currency_limit(limit)?;

    match peer
        .exchange(&NetworkMessage::GetPublicCurrencies { start, limit })
        .await?
    {
        NetworkMessage::PublicCurrencies { states, next_start } => {
            validate_public_currency_page(start, limit, &states, next_start)?;
            Ok((states, next_start))
        }
        _ => Err(NetworkError::UnexpectedMessage),
    }
}

async fn request_public_currency_checkpoint_proof(
    peer: &QuicPeer,
) -> Result<Option<PublicCurrencyCheckpointProof>, NetworkError> {
    match peer
        .exchange(&NetworkMessage::GetPublicCurrencyCheckpoint)
        .await?
    {
        NetworkMessage::PublicCurrencyCheckpointProof { proof } => Ok(Some(proof)),
        NetworkMessage::NoPublicCurrencyCheckpoint => Ok(None),
        _ => Err(NetworkError::UnexpectedMessage),
    }
}

async fn request_public_currency_summary(
    peer: &QuicPeer,
) -> Result<crate::PublicCurrencySummary, NetworkError> {
    match peer
        .exchange(&NetworkMessage::GetPublicCurrencySummary)
        .await?
    {
        NetworkMessage::PublicCurrencySummary { summary } => Ok(summary),
        _ => Err(NetworkError::UnexpectedMessage),
    }
}

fn max_public_sync_states() -> u64 {
    let element_size = std::mem::size_of::<PublicCurrencyState>();
    debug_assert!(element_size > 0);

    u64::try_from(MAX_PUBLIC_SYNC_STATE_BYTES / element_size).unwrap_or(u64::MAX)
}

fn validate_public_currency_page(
    start: CurrencyAddress,
    limit: u16,
    states: &[crate::PublicCurrencyState],
    next_start: Option<CurrencyAddress>,
) -> Result<(), NetworkError> {
    if states.len() > usize::from(limit) {
        return Err(NetworkError::InvalidPublicCurrencyPage);
    }

    let mut previous = None;
    for state in states {
        if state.address < start {
            return Err(NetworkError::InvalidPublicCurrencyPage);
        }

        if let Some(previous_address) = previous
            && state.address <= previous_address
        {
            return Err(NetworkError::InvalidPublicCurrencyPage);
        }

        previous = Some(state.address);
    }

    if let Some(next) = next_start {
        if states.len() != usize::from(limit) {
            return Err(NetworkError::InvalidPublicCurrencyPage);
        }

        let current = states.last().map(|state| state.address).unwrap_or(start);
        if next <= current {
            return Err(NetworkError::InvalidPublicCurrencyCursor { current, next });
        }
    }

    Ok(())
}

fn validate_synced_page(
    start: CurrencyAddress,
    limit: u16,
    frontier: u64,
    states: &[crate::PublicCurrencyState],
    next_start: Option<CurrencyAddress>,
) -> Result<(), NetworkError> {
    validate_public_currency_page(start, limit, states, next_start)?;

    if let Some(next) = next_start
        && next.value() >= frontier
    {
        let current = states.last().map(|state| state.address).unwrap_or(start);
        return Err(NetworkError::InvalidPublicCurrencyCursor { current, next });
    }

    Ok(())
}

fn public_currency_page_response(
    view: &PublicCurrencyView,
    start: CurrencyAddress,
    limit: u16,
) -> Result<NetworkMessage, NetworkError> {
    validate_public_currency_limit(limit)?;
    let start_index = view.states.partition_point(|state| state.address < start);
    let end_index = start_index
        .saturating_add(usize::from(limit))
        .min(view.states.len());
    let states = view.states[start_index..end_index].to_vec();
    let next_start = view.states.get(end_index).map(|state| state.address);
    Ok(NetworkMessage::PublicCurrencies { states, next_start })
}
