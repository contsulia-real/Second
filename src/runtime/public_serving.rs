use super::*;

impl PublicNetworkContext {
    fn load_public_snapshot(
        &self,
        needs_view: bool,
    ) -> Result<RuntimePublicSnapshot, NetworkError> {
        let mut cache = self
            .public_view_cache
            .lock()
            .map_err(|_| NetworkError::PublicStateSource("public cache poisoned".to_owned()))?;
        match &self.backend {
            NodeStateBackend::Full(store) => {
                let persisted = store
                    .load_shared()
                    .map_err(|error| NetworkError::PublicStateSource(format!("{error:?}")))?
                    .ok_or_else(|| {
                        NetworkError::PublicStateSource("snapshot missing".to_owned())
                    })?;
                let view = if needs_view {
                    if cache
                        .as_ref()
                        .is_none_or(|(generation, _)| *generation != persisted.generation)
                    {
                        *cache = Some((
                            persisted.generation,
                            Arc::new(
                                crate::PublicCurrencyView::new(
                                    persisted.state.public_currency_summary(),
                                    persisted.state.public_currency_states(),
                                )
                                .map_err(NetworkError::PublicState)?,
                            ),
                        ));
                    }
                    cache.as_ref().map(|(_, view)| Arc::clone(view))
                } else {
                    None
                };
                Ok(RuntimePublicSnapshot {
                    view,
                    checkpoint_proof: persisted.public_checkpoint_proof.clone(),
                    latest_delta: persisted.latest_public_delta.clone(),
                })
            }
            NodeStateBackend::Public(store) => {
                let persisted = store
                    .load_shared()
                    .map_err(|error| NetworkError::PublicStateSource(format!("{error:?}")))?
                    .ok_or_else(|| {
                        NetworkError::PublicStateSource("snapshot missing".to_owned())
                    })?;
                let view = if needs_view {
                    if cache
                        .as_ref()
                        .is_none_or(|(generation, _)| *generation != persisted.generation)
                    {
                        *cache = Some((
                            persisted.generation,
                            Arc::new(persisted.view.clone().ok_or_else(|| {
                                NetworkError::PublicStateSource(
                                    "public state not synchronized".to_owned(),
                                )
                            })?),
                        ));
                    }
                    cache.as_ref().map(|(_, view)| Arc::clone(view))
                } else {
                    None
                };
                Ok(RuntimePublicSnapshot {
                    view,
                    checkpoint_proof: persisted.checkpoint_proof.clone(),
                    latest_delta: None,
                })
            }
        }
    }
}

pub(super) async fn serve_managed_peer(
    peer: QuicPeer,
    context: PublicNetworkContext,
    peer_lease: PeerLease,
    permit: ActiveConnectionPermit,
    outbound_client: Option<QuicClient>,
    first_request: Option<QuicRequestStream>,
) {
    let _peer_lease = peer_lease;
    let service_context = Arc::new((context, permit));

    // Only inbound peers need to advertise a listening endpoint; outbound dials
    // already persisted their pinned record and bootstrap performs discovery.
    if outbound_client.is_none() {
        let refresh_peer = peer.clone();
        let refresh_context = Arc::clone(&service_context);
        tokio::spawn(async move {
            if let Ok(records) = client_peer_records(&refresh_peer, 1).await
                && let Some(record) = records
                    .into_iter()
                    .find(|record| record.node_id() == refresh_peer.remote_node_id())
            {
                let persisted = tokio::task::spawn_blocking(move || {
                    refresh_context.0.peer_store.record_authenticated(&record)
                })
                .await
                .is_ok_and(|result| result.is_ok());
                if !persisted {
                    refresh_peer.close_with_reason(b"peer store failure");
                }
            }
        });
    }
    let _outbound_client = outbound_client;

    let public_context = Arc::clone(&service_context);
    let load_public_snapshot =
        Arc::new(move |needs_view| public_context.0.load_public_snapshot(needs_view));
    let transition_context = Arc::clone(&service_context);
    let load_transition_proof = Arc::new(move |version| {
        load_transition_proof(transition_context.0.backend.full_store(), version)
    });
    let recovery_loader = service_context.0.backend.full_store().map(|_| {
        let recovery_context = Arc::clone(&service_context);
        Arc::new(move || {
            let store = recovery_context.0.backend.full_store().ok_or_else(|| {
                NetworkError::PublicStateSource("recovery unavailable".to_owned())
            })?;
            store
                .load_shared()
                .map_err(|error| NetworkError::PublicStateSource(format!("{error:?}")))?
                .ok_or_else(|| NetworkError::PublicStateSource("snapshot missing".to_owned()))
        }) as Arc<dyn Fn() -> Result<RuntimeNetworkSnapshot, NetworkError> + Send + Sync>
    });
    let services = PublicNetworkServices::new(
        service_context.0.peer_store.clone(),
        service_context.0.local_node_id,
        service_context.0.local_peer_record.clone(),
        service_context.0.state_recovery_provider.clone(),
        load_public_snapshot,
        load_transition_proof,
        recovery_loader,
    );
    let result = match first_request {
        Some(first_request) => {
            serve_public_network_connection_from_request(&peer, services, first_request).await
        }
        None => serve_public_network_connection(&peer, services).await,
    };
    let _ = result;
}
