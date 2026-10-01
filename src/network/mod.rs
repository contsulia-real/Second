mod codec;
mod identity;
mod peer_manager;
mod peer_record;
mod peer_store;
mod quic;
mod recovery;
mod session;

use std::fmt;

use crate::{
    CertifiedPublicCurrencyCheckpoint, CertifiedStateRecoveryCheckpoint, CurrencyAddress,
    FinalityError, PublicCheckpointError, PublicCurrencyCheckpointProof, PublicCurrencyState,
    PublicCurrencySummary, PublicCurrencyView, PublicStateError, StateRecoveryCheckpointProof,
    StateRecoveryPayload, ValidatorId,
};

pub use codec::{decode_network_message, encode_network_message};
pub use identity::QuicTransportIdentity;
pub(crate) use peer_manager::{PeerDirection, PeerLease, PeerManager, PeerRegistrationError};
pub(crate) use peer_record::validate_peer_limit;
pub use peer_record::{MAX_PEER_CERTIFICATE_SIZE, MAX_PEER_RECORDS, PeerRecord};
pub(crate) use peer_store::PeerStore;
pub use quic::{QuicClient, QuicPeer, QuicRequestStream, QuicServer, SECOND_QUIC_SERVER_NAME};
pub use recovery::{MAX_STATE_RECOVERY_CHUNK_SIZE, client_fetch_state_recovery};
pub(crate) use recovery::{
    StateRecoveryProvider, StateRecoveryProviderHandle, new_state_recovery_provider_handle,
    state_recovery_response, validate_chunk_limit,
};
pub use session::{
    client_peer_records, client_ping, client_public_currency_checkpoint_proof,
    client_public_currency_page, client_public_currency_summary,
    client_sync_certified_public_currency_view, client_sync_public_currency_view,
    serve_ping_session, serve_public_currency_connection,
};
pub(crate) use session::{
    client_sync_certified_public_currency_view_from_checkpoint, serve_public_network_connection,
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

#[derive(Clone)]
pub struct RemoteStateRecoveryPayload {
    pub remote_node_id: NodeId,
    pub payload: StateRecoveryPayload,
    pub checkpoint: CertifiedStateRecoveryCheckpoint,
    pub encoded_payload_len: usize,
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
    GetPeers {
        limit: u16,
    },
    Peers {
        records: Vec<PeerRecord>,
    },
    GetStateRecoveryManifest {
        validator_id: ValidatorId,
        signature: [u8; 64],
    },
    StateRecoveryManifest {
        proof: StateRecoveryCheckpointProof,
        payload_len: u64,
    },
    NoStateRecoveryCheckpoint,
    GetStateRecoveryChunk {
        validator_id: ValidatorId,
        checkpoint_digest: [u8; 32],
        offset: u64,
        limit: u32,
        signature: [u8; 64],
    },
    StateRecoveryChunk {
        checkpoint_digest: [u8; 32],
        offset: u64,
        bytes: Vec<u8>,
    },
    StateRecoveryDenied,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NetworkError {
    Transport(String),
    TransportIdentity(String),
    InvalidTransportIdentity,
    PeerAuthenticationFailed(NodeId),
    UnexpectedPeerIdentity {
        expected: NodeId,
        actual: NodeId,
    },
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
    InvalidPeerRecord,
    InvalidPeerAddressFamily(u8),
    InvalidPeerLimit {
        requested: u16,
        maximum: u16,
    },
    TooManyPeerRecords {
        announced: usize,
        maximum: usize,
    },
    PeerStore(String),
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
    StateRecoveryUnauthorized,
    MissingStateRecoveryCheckpoint,
    InvalidStateRecoveryProof,
    InvalidStateRecoveryChunk,
    InvalidStateRecoveryPayload,
    StateRecoveryFinality(FinalityError),
    StateRecoveryPayloadTooLarge {
        announced: u64,
        maximum: u64,
    },
    InvalidStateRecoveryChunkLimit {
        requested: u32,
        maximum: u32,
    },
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
