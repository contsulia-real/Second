//! Public peer discovery and durable outbound connections.
use super::*;
use std::future::{Future, poll_fn};
use std::task::Poll;

const PUBLIC_DISCOVERY_CONCURRENCY: usize = 4;

impl NodeRuntime {
    async fn known_active_peer_count(&self) -> Result<usize, NodeRuntimeError> {
        Ok(self
            .peer_store
            .recent_async(MAX_LOCAL_PEER_CANDIDATES, vec![self.node_id()])
            .await?
            .into_iter()
            .filter(|record| self.peer_manager.peer(record.node_id()).is_some())
            .count())
    }

    pub async fn bootstrap(
        &self,
        bootstrap_records: &[PeerRecord],
        target_connections: usize,
    ) -> Result<usize, NodeRuntimeError> {
        if target_connections > MAX_ACTIVE_CONNECTIONS {
            return Err(NodeRuntimeError::ConnectionCapacityReached {
                maximum: MAX_ACTIVE_CONNECTIONS,
            });
        }

        let mut candidates = VecDeque::new();
        candidates.extend(
            self.peer_store
                .recent_async(MAX_LOCAL_PEER_CANDIDATES, vec![self.node_id()])
                .await?,
        );
        candidates.extend(
            bootstrap_records
                .iter()
                .filter(|record| record.node_id() != self.node_id())
                .cloned(),
        );

        let mut attempted = HashSet::new();
        let mut probes = Vec::new();
        loop {
            let active = self.known_active_peer_count().await?;
            while probes.len() < PUBLIC_DISCOVERY_CONCURRENCY
                && active + probes.len() < target_connections
            {
                let next = candidates
                    .iter()
                    .position(|record| !probes.iter().any(|(node, _)| *node == record.node_id()));
                let Some(record) = next.and_then(|index| candidates.remove(index)) else {
                    break;
                };
                if !attempted.insert(record.clone()) {
                    continue;
                }
                probes.push((record.node_id(), Box::pin(self.discover_peer(record))));
            }
            let Some(result) = poll_fn(|cx| {
                for index in 0..probes.len() {
                    if let Poll::Ready(result) = probes[index].1.as_mut().poll(cx) {
                        drop(probes.swap_remove(index));
                        return Poll::Ready(Some(result));
                    }
                }
                if probes.is_empty() {
                    Poll::Ready(None)
                } else {
                    Poll::Pending
                }
            })
            .await
            else {
                break;
            };
            let (peer, records) = match result {
                Ok(result) => result,
                Err(error @ NodeRuntimeError::Network(NetworkError::PeerStore(_))) => {
                    return Err(error);
                }
                Err(NodeRuntimeError::ConnectionCapacityReached { .. }) => break,
                Err(_) => continue,
            };

            for record in records {
                if record.node_id() == peer.remote_node_id() {
                    let cache = self.peer_store.clone();
                    tokio::task::spawn_blocking(move || cache.record_authenticated(&record))
                        .await
                        .map_err(NodeRuntimeError::RuntimeTaskFailed)??;
                } else if record.node_id() != self.node_id() {
                    candidates.push_back(record);
                }
            }
        }

        self.known_active_peer_count().await
    }

    async fn discover_peer(
        &self,
        record: PeerRecord,
    ) -> Result<(QuicPeer, Vec<PeerRecord>), NodeRuntimeError> {
        let peer = match self.peer_manager.peer(record.node_id()) {
            Some(peer) => peer,
            None => self.dial(&record).await?,
        };
        let records = match client_peer_records(&peer, MAX_PEER_RECORDS).await {
            Ok(records) => records,
            Err(_) => {
                peer.close_with_reason(b"peer discovery failed");
                Vec::new()
            }
        };
        Ok((peer, records))
    }

    pub async fn dial(&self, record: &PeerRecord) -> Result<QuicPeer, NodeRuntimeError> {
        let permit = ActiveConnectionPermit::try_acquire(&self.active_connections).ok_or(
            NodeRuntimeError::ConnectionCapacityReached {
                maximum: MAX_ACTIVE_CONNECTIONS,
            },
        )?;

        let (client, permit) = match self
            .server
            .shared_client(record.address(), record.certificate_der())
        {
            Ok(Some(client)) => (Ok(client), permit),
            Err(error) => (Err(error), permit),
            Ok(None) => {
                let dial_record = record.clone();
                let identity = self.transport_identity.clone();
                // A different address family needs its own socket and blocking worker.
                tokio::task::spawn_blocking(move || {
                    (
                        QuicClient::new(
                            outbound_bind_address(dial_record.address()),
                            dial_record.certificate_der(),
                            identity,
                        ),
                        permit,
                    )
                })
                .await
                .map_err(NodeRuntimeError::RuntimeTaskFailed)?
            }
        };
        let client = match client {
            Ok(client) => client,
            Err(error) => {
                let cache = self.peer_store.clone();
                let record = record.clone();
                tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    cache.record_failure(&record)
                })
                .await
                .map_err(NodeRuntimeError::RuntimeTaskFailed)??;
                return Err(error.into());
            }
        };
        let peer = match client
            .connect_expected(record.address(), record.node_id())
            .await
        {
            Ok(peer) => peer,
            Err(error) => {
                let cache = self.peer_store.clone();
                let record = record.clone();
                tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    let _client = client;
                    cache.record_failure(&record)
                })
                .await
                .map_err(NodeRuntimeError::RuntimeTaskFailed)??;
                return Err(error.into());
            }
        };
        let peer_lease = match self.peer_manager.register(&peer, PeerDirection::Outbound) {
            Ok(lease) => lease,
            Err(PeerRegistrationError::Duplicate(node_id)) => {
                peer.close_with_reason(b"duplicate peer");
                return self
                    .peer_manager
                    .peer(node_id)
                    .ok_or(NodeRuntimeError::DuplicatePeer(node_id));
            }
            Err(PeerRegistrationError::SelfConnection) => {
                peer.close_with_reason(b"self connection");
                return Err(NodeRuntimeError::SelfConnection);
            }
        };

        let cache = self.peer_store.clone();
        let cached_record = record.clone();
        let (cached, peer, peer_lease, permit, client) = tokio::task::spawn_blocking(move || {
            let cached = cache.record_authenticated(&cached_record);
            (cached, peer, peer_lease, permit, client)
        })
        .await
        .map_err(NodeRuntimeError::RuntimeTaskFailed)?;
        if let Err(error) = cached {
            peer.close_with_reason(b"peer store failure");
            drop(peer_lease);
            return Err(error.into());
        }

        let active_peer = peer.clone();
        tokio::spawn(serve_managed_peer(
            peer,
            self.public_network_context(),
            peer_lease,
            permit,
            Some(client),
            None,
        ));

        Ok(active_peer)
    }

    pub(super) async fn maintain_peers(
        &self,
        bootstrap_records: &[PeerRecord],
    ) -> Result<(), NodeRuntimeError> {
        tokio::try_join!(
            self.maintain_validator_bft_connections(bootstrap_records),
            self.maintain_public_peers(bootstrap_records),
        )?;
        Ok(())
    }

    async fn maintain_public_peers(
        &self,
        bootstrap_records: &[PeerRecord],
    ) -> Result<(), NodeRuntimeError> {
        let mut retry_delay = PEER_RETRY_INITIAL_DELAY;
        let mut retry_at = tokio::time::Instant::now();
        let mut previous_active = 0;

        loop {
            let active_before = self.known_active_peer_count().await?;
            // Discovery backoff must not delay replacement of a lost live peer.
            if active_before < previous_active {
                retry_delay = PEER_RETRY_INITIAL_DELAY;
                retry_at = tokio::time::Instant::now();
            }
            previous_active = active_before;
            if active_before >= DEFAULT_ACTIVE_PEER_TARGET {
                retry_delay = PEER_RETRY_INITIAL_DELAY;
                retry_at = tokio::time::Instant::now();
            } else if tokio::time::Instant::now() >= retry_at {
                let active_after = self
                    .bootstrap(bootstrap_records, DEFAULT_ACTIVE_PEER_TARGET)
                    .await?;
                previous_active = active_after;
                if active_after > active_before {
                    retry_delay = PEER_RETRY_INITIAL_DELAY;
                    retry_at = tokio::time::Instant::now() + retry_delay;
                } else {
                    retry_at = tokio::time::Instant::now() + retry_delay;
                    retry_delay = retry_delay
                        .checked_mul(2)
                        .unwrap_or(PEER_RETRY_MAX_DELAY)
                        .min(PEER_RETRY_MAX_DELAY);
                }
            }

            tokio::time::sleep(PEER_MAINTENANCE_INTERVAL).await;
        }
    }
}
