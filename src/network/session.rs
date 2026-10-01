use crate::{
    CertifiedPublicCurrencyCheckpoint, CurrencyAddress, PublicCurrencyCheckpointProof,
    PublicCurrencyState, PublicCurrencyView, SecondState, ValidatorSet,
};

use super::quic::{QuicPeer, QuicRequestStream};

const MAX_PUBLIC_SYNC_STATE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy)]
pub(crate) struct PublicNetworkServices<'a> {
    peer_store: &'a PeerStore,
    local_node_id: NodeId,
    local_peer_record: Option<&'a PeerRecord>,
    state_recovery_provider: &'a StateRecoveryProviderHandle,
}

impl<'a> PublicNetworkServices<'a> {
    pub(crate) const fn new(
        peer_store: &'a PeerStore,
        local_node_id: NodeId,
        local_peer_record: Option<&'a PeerRecord>,
        state_recovery_provider: &'a StateRecoveryProviderHandle,
    ) -> Self {
        Self {
            peer_store,
            local_node_id,
            local_peer_record,
            state_recovery_provider,
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
    serve_public_connection(peer, state, checkpoint_proof, None, None).await
}

pub(crate) async fn serve_public_network_connection(
    peer: &QuicPeer,
    state: &SecondState,
    checkpoint_proof: Option<&PublicCurrencyCheckpointProof>,
    services: PublicNetworkServices<'_>,
) -> Result<NodeId, NetworkError> {
    serve_public_connection(peer, state, checkpoint_proof, Some(services), None).await
}

pub(crate) async fn serve_public_network_connection_from_request(
    peer: &QuicPeer,
    state: &SecondState,
    checkpoint_proof: Option<&PublicCurrencyCheckpointProof>,
    services: PublicNetworkServices<'_>,
    first_request: QuicRequestStream,
) -> Result<NodeId, NetworkError> {
    serve_public_connection(
        peer,
        state,
        checkpoint_proof,
        Some(services),
        Some(first_request),
    )
    .await
}

async fn serve_public_connection(
    peer: &QuicPeer,
    state: &SecondState,
    checkpoint_proof: Option<&PublicCurrencyCheckpointProof>,
    services: Option<PublicNetworkServices<'_>>,
    mut first_request: Option<QuicRequestStream>,
) -> Result<NodeId, NetworkError> {
    if checkpoint_proof
        .is_some_and(|proof| proof.checkpoint().summary() != &state.public_currency_summary())
    {
        return Err(NetworkError::CheckpointDoesNotMatchServedState);
    }

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

        let recovery_response = services.as_ref().and_then(|services| {
            state_recovery_response(peer, request.message(), services.state_recovery_provider)
        });
        let response = match recovery_response {
            Some(response) => response?,
            None => match request.message() {
                NetworkMessage::Ping { nonce } => NetworkMessage::Pong { nonce: *nonce },
                NetworkMessage::GetPublicCurrencies { start, limit } => {
                    public_currency_page_response(state, *start, *limit)?
                }
                NetworkMessage::GetPublicCurrencySummary => NetworkMessage::PublicCurrencySummary {
                    summary: state.public_currency_summary(),
                },
                NetworkMessage::GetPublicCurrencyCheckpoint => checkpoint_proof
                    .cloned()
                    .map(|proof| NetworkMessage::PublicCurrencyCheckpointProof { proof })
                    .unwrap_or(NetworkMessage::NoPublicCurrencyCheckpoint),
                NetworkMessage::GetPeers { limit } => {
                    validate_peer_limit(*limit)?;
                    let services = services.as_ref().ok_or(NetworkError::UnexpectedMessage)?;
                    let mut records = Vec::with_capacity(usize::from(*limit));
                    if let Some(record) = services.local_peer_record {
                        records.push(record.clone());
                    }
                    let remaining = limit.saturating_sub(records.len() as u16);
                    if remaining > 0 {
                        records.extend(
                            services.peer_store.recent(
                                remaining,
                                &[services.local_node_id, peer.remote_node_id()],
                            ),
                        );
                    }
                    NetworkMessage::Peers { records }
                }
                _ => return Err(NetworkError::UnexpectedMessage),
            },
        };

        request.respond(&response).await?;
    }
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
    state: &SecondState,
    start: CurrencyAddress,
    limit: u16,
) -> Result<NetworkMessage, NetworkError> {
    let page = state.public_currency_page(start, limit)?;
    Ok(NetworkMessage::PublicCurrencies {
        states: page.states,
        next_start: page.next_start,
    })
}
