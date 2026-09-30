use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use super::NodeId;

#[derive(Clone)]
pub(crate) struct PeerManager {
    inner: Arc<PeerManagerInner>,
}

struct PeerManagerInner {
    local_node_id: NodeId,
    connected: Mutex<HashSet<NodeId>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PeerRegistrationError {
    SelfConnection,
    Duplicate(NodeId),
}

pub(crate) struct PeerLease {
    inner: Arc<PeerManagerInner>,
    node_id: NodeId,
}

impl PeerManager {
    pub(crate) fn new(local_node_id: NodeId) -> Self {
        Self {
            inner: Arc::new(PeerManagerInner {
                local_node_id,
                connected: Mutex::new(HashSet::new()),
            }),
        }
    }

    pub(crate) fn register(&self, node_id: NodeId) -> Result<PeerLease, PeerRegistrationError> {
        if node_id == self.inner.local_node_id {
            return Err(PeerRegistrationError::SelfConnection);
        }

        let mut connected = self
            .inner
            .connected
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !connected.insert(node_id) {
            return Err(PeerRegistrationError::Duplicate(node_id));
        }

        Ok(PeerLease {
            inner: Arc::clone(&self.inner),
            node_id,
        })
    }
}

impl Drop for PeerLease {
    fn drop(&mut self) {
        self.inner
            .connected
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.node_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_peer_identity_is_unique_and_released_with_lease() {
        let local = NodeId::from_bytes([1; 32]);
        let remote = NodeId::from_bytes([2; 32]);
        let manager = PeerManager::new(local);

        assert_eq!(
            manager.register(local).err(),
            Some(PeerRegistrationError::SelfConnection)
        );

        let lease = manager.register(remote).unwrap();
        assert_eq!(
            manager.register(remote).err(),
            Some(PeerRegistrationError::Duplicate(remote))
        );

        drop(lease);
        assert!(manager.register(remote).is_ok());
    }
}
