use std::net::SocketAddr;

use super::{NetworkError, NodeId};

pub const MAX_PEER_RECORDS: u16 = 32;
pub const MAX_PEER_CERTIFICATE_SIZE: usize = 1024;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PeerRecord {
    node_id: NodeId,
    address: SocketAddr,
    certificate_der: Vec<u8>,
}

impl PeerRecord {
    pub fn new(
        node_id: NodeId,
        address: SocketAddr,
        certificate_der: Vec<u8>,
    ) -> Result<Self, NetworkError> {
        Self::validate_address(address)?;
        if certificate_der.is_empty() || certificate_der.len() > MAX_PEER_CERTIFICATE_SIZE {
            return Err(NetworkError::InvalidPeerRecord);
        }

        Ok(Self {
            node_id,
            address,
            certificate_der,
        })
    }

    pub fn validate_address(address: SocketAddr) -> Result<(), NetworkError> {
        if address.port() == 0
            || address.ip().is_unspecified()
            || matches!(
                address,
                SocketAddr::V6(address) if address.flowinfo() != 0 || address.scope_id() != 0
            )
        {
            Err(NetworkError::InvalidPeerRecord)
        } else {
            Ok(())
        }
    }

    pub const fn node_id(&self) -> NodeId {
        self.node_id
    }

    pub const fn address(&self) -> SocketAddr {
        self.address
    }

    pub fn certificate_der(&self) -> &[u8] {
        &self.certificate_der
    }
}

pub(crate) fn validate_peer_limit(limit: u16) -> Result<(), NetworkError> {
    if limit == 0 || limit > MAX_PEER_RECORDS {
        Err(NetworkError::InvalidPeerLimit {
            requested: limit,
            maximum: MAX_PEER_RECORDS,
        })
    } else {
        Ok(())
    }
}
