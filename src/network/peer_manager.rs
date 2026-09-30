use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::{NodeId, QuicPeer};

#[derive(Clone)]
pub(crate) struct PeerManager {
    inner: Arc<PeerManagerInner>,
}

struct PeerManagerInner {
    local_node_id: NodeId,
    connected: Mutex<HashMap<NodeId, ActivePeer>>,
}

struct ActivePeer {
    direction: PeerDirection,
    token: Arc<()>,
    peer: Option<QuicPeer>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PeerDirection {
    Inbound,
    Outbound,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PeerRegistrationError {
    SelfConnection,
    Duplicate(NodeId),
}

pub(crate) struct PeerLease {
    inner: Arc<PeerManagerInner>,
    node_id: NodeId,
    token: Arc<()>,
}

impl PeerManager {
    pub(crate) fn new(local_node_id: NodeId) -> Self {
        Self {
            inner: Arc::new(PeerManagerInner {
                local_node_id,
                connected: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub(crate) fn register(
        &self,
        peer: &QuicPeer,
        direction: PeerDirection,
    ) -> Result<PeerLease, PeerRegistrationError> {
        self.register_entry(peer.remote_node_id(), direction, Some(peer.clone()))
    }

    pub(crate) fn peer(&self, node_id: NodeId) -> Option<QuicPeer> {
        self.inner
            .connected
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&node_id)
            .and_then(|active| active.peer.clone())
    }

    pub(crate) fn len(&self) -> usize {
        self.inner
            .connected
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }

    fn register_entry(
        &self,
        node_id: NodeId,
        direction: PeerDirection,
        peer: Option<QuicPeer>,
    ) -> Result<PeerLease, PeerRegistrationError> {
        if node_id == self.inner.local_node_id {
            return Err(PeerRegistrationError::SelfConnection);
        }

        let token = Arc::new(());
        let mut replaced_peer = None;
        let mut connected = self
            .inner
            .connected
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        match connected.get_mut(&node_id) {
            None => {
                connected.insert(
                    node_id,
                    ActivePeer {
                        direction,
                        token: Arc::clone(&token),
                        peer,
                    },
                );
            }
            Some(active) => {
                let preferred = preferred_direction(self.inner.local_node_id, node_id);
                if active.direction == preferred || direction != preferred {
                    return Err(PeerRegistrationError::Duplicate(node_id));
                }

                replaced_peer = active.peer.take();
                *active = ActivePeer {
                    direction,
                    token: Arc::clone(&token),
                    peer,
                };
            }
        }

        drop(connected);
        if let Some(peer) = replaced_peer {
            peer.close_with_reason(b"superseded peer");
        }

        Ok(PeerLease {
            inner: Arc::clone(&self.inner),
            node_id,
            token,
        })
    }
}

impl Drop for PeerLease {
    fn drop(&mut self) {
        let mut connected = self
            .inner
            .connected
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        if connected
            .get(&self.node_id)
            .is_some_and(|active| Arc::ptr_eq(&active.token, &self.token))
        {
            connected.remove(&self.node_id);
        }
    }
}

fn preferred_direction(local: NodeId, remote: NodeId) -> PeerDirection {
    if local < remote {
        PeerDirection::Outbound
    } else {
        PeerDirection::Inbound
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_peer_prefers_one_symmetric_simultaneous_dial_direction() {
        let low = NodeId::from_bytes([1; 32]);
        let high = NodeId::from_bytes([2; 32]);

        assert_eq!(preferred_direction(low, high), PeerDirection::Outbound);
        assert_eq!(preferred_direction(high, low), PeerDirection::Inbound);

        let low_manager = PeerManager::new(low);
        let stale = low_manager
            .register_entry(high, PeerDirection::Inbound, None)
            .unwrap();
        let preferred = low_manager
            .register_entry(high, PeerDirection::Outbound, None)
            .unwrap();
        drop(stale);

        assert_eq!(
            low_manager
                .register_entry(high, PeerDirection::Inbound, None)
                .err(),
            Some(PeerRegistrationError::Duplicate(high))
        );

        drop(preferred);
        assert!(
            low_manager
                .register_entry(high, PeerDirection::Inbound, None)
                .is_ok()
        );
    }

    #[test]
    fn self_connection_is_rejected() {
        let local = NodeId::from_bytes([1; 32]);
        let manager = PeerManager::new(local);

        assert_eq!(
            manager
                .register_entry(local, PeerDirection::Inbound, None)
                .err(),
            Some(PeerRegistrationError::SelfConnection)
        );
    }
}
