mod codec;
mod identity;
mod peer_manager;
mod quic;
mod session;

use std::fmt;

use crate::{
    CertifiedPublicCurrencyCheckpoint, CurrencyAddress, PublicCheckpointError,
    PublicCurrencyCheckpointProof, PublicCurrencyState, PublicCurrencySummary, PublicCurrencyView,
    PublicStateError,
};

pub use codec::{decode_network_message, encode_network_message};
pub use identity::QuicTransportIdentity;
pub(crate) use peer_manager::{PeerManager, PeerRegistrationError};
pub use quic::{QuicClient, QuicPeer, QuicRequestStream, QuicServer, SECOND_QUIC_SERVER_NAME};
pub use session::{
    client_ping, client_public_currency_checkpoint_proof, client_public_currency_page,
    client_public_currency_summary, client_sync_certified_public_currency_view,
    client_sync_public_currency_view, serve_ping_session, serve_public_currency_connection,
};

pub const CURRENT_NETWORK_PROTOCOL_VERSION: u32 = 1;
pub const MAX_NETWORK_FRAME_SIZE: usize = 64 * 1024;
pub const MAX_PUBLIC_CURRENCY_PAGE: u16 = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct NodeId([u8; 32]);

impl NodeId {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn from_u64(value: u64) -> Self {
        let mut bytes = [0_u8; 32];
        bytes[24..].copy_from_slice(&value.to_be_bytes());
        Self(bytes)
    }

    pub const fn to_bytes(self) -> [u8; 32] {
        self.0
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicCurrencyPage {
    pub states: Vec<PublicCurrencyState>,
    pub next_start: Option<CurrencyAddress>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemotePublicCurrencyPage {
    pub remote_node_id: NodeId,
    pub states: Vec<PublicCurrencyState>,
    pub next_start: Option<CurrencyAddress>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemotePublicCurrencyView {
    pub remote_node_id: NodeId,
    pub view: PublicCurrencyView,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteCertifiedPublicCurrencyView {
    pub remote_node_id: NodeId,
    pub view: PublicCurrencyView,
    pub checkpoint: CertifiedPublicCurrencyCheckpoint,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemotePublicCurrencySummary {
    pub remote_node_id: NodeId,
    pub summary: PublicCurrencySummary,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NetworkMessage {
    Hello {
        node_id: NodeId,
        signature: [u8; 64],
    },
    Ping {
        nonce: u64,
    },
    Pong {
        nonce: u64,
    },
    GetPublicCurrencies {
        start: CurrencyAddress,
        limit: u16,
    },
    PublicCurrencies {
        states: Vec<PublicCurrencyState>,
        next_start: Option<CurrencyAddress>,
    },
    GetPublicCurrencySummary,
    PublicCurrencySummary {
        summary: PublicCurrencySummary,
    },
    GetPublicCurrencyCheckpoint,
    PublicCurrencyCheckpointProof {
        proof: PublicCurrencyCheckpointProof,
    },
    NoPublicCurrencyCheckpoint,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NetworkError {
    Transport(String),
    TransportIdentity(String),
    InvalidTransportIdentity,
    PeerAuthenticationFailed(NodeId),
    InvalidMagic,
    UnsupportedProtocolVersion {
        expected: u32,
        actual: u32,
    },
    FrameTooLarge {
        announced: usize,
        maximum: usize,
    },
    EmptyPayload,
    UnknownMessageType(u8),
    InvalidMessageLength {
        message_type: u8,
        expected: usize,
        actual: usize,
    },
    InvalidCheckpointProof,
    InvalidCurrencyRole(u8),
    InvalidBoolean(u8),
    InvalidCursorFlag(u8),
    InvalidPublicCurrencyLimit {
        requested: u16,
        maximum: u16,
    },
    TooManyPublicCurrencyStates {
        announced: usize,
        maximum: usize,
    },
    InvalidPublicCurrencyPage,
    InvalidPublicCurrencyCursor {
        current: CurrencyAddress,
        next: CurrencyAddress,
    },
    SynchronizedCurrencyCountExceeded {
        claimed: u64,
        actual: u64,
    },
    PublicCurrencySyncTooLarge {
        announced: u64,
        maximum: u64,
    },
    PublicCurrencySyncAllocationFailed {
        requested: u64,
    },
    PublicState(PublicStateError),
    PublicCheckpoint(PublicCheckpointError),
    MissingPublicCurrencyCheckpoint,
    StalePublicCurrencyCheckpoint {
        minimum_epoch: u64,
        actual_epoch: u64,
    },
    CheckpointDoesNotMatchServedState,
    UnexpectedMessage,
    NonceMismatch {
        expected: u64,
        actual: u64,
    },
}

pub(crate) fn validate_public_currency_limit(limit: u16) -> Result<(), NetworkError> {
    if limit == 0 || limit > MAX_PUBLIC_CURRENCY_PAGE {
        Err(NetworkError::InvalidPublicCurrencyLimit {
            requested: limit,
            maximum: MAX_PUBLIC_CURRENCY_PAGE,
        })
    } else {
        Ok(())
    }
}
