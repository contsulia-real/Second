pub(crate) mod account_query;
mod bft;
pub use account_query::{
    AccountCurrencyRange, AccountPaymentAddress, AccountTransfer, AccountView,
    MAX_ACCOUNT_QUERY_PAGE, client_account_query, client_account_view,
};
mod bft_codec;
mod codec;
mod governance;
mod identity;
mod peer_manager;
mod peer_record;
mod peer_store;
mod quic;
mod recovery;
#[cfg(test)]
pub(crate) use recovery::fetch_from_current_member;
mod session;
mod submission;
mod task_status;

use std::fmt;

use crate::{
    BftError, CertifiedPublicCurrencyCheckpoint, CertifiedStateRecoveryCheckpoint, CurrencyAddress,
    FinalityError, LegalTaskStatus, LegalTaskSubmissionOutcome, PublicCheckpointError,
    PublicCurrencyCheckpointProof, PublicCurrencyDelta, PublicCurrencyState, PublicCurrencySummary,
    PublicCurrencyView, PublicStateError, StateRecoveryCheckpointProof, StateRecoveryPayload,
    TaskId, ValidatorId, ValidatorSetTransitionProof,
};

pub(crate) use bft::{
    SharedValidatorBftAuthority, ValidatorBftAuthority,
    authenticate_validator_bft_peer_with_authority, serve_validator_bft_connection_from_request,
};
pub use bft::{ValidatorBftPeer, authenticate_validator_bft_peer, serve_validator_bft_connection};
pub(crate) use bft_codec::MAX_PREPARED_TASK_SOURCE_CHUNK_SIZE;
pub use bft_codec::{BftNetworkMessage, decode_bft_network_message, encode_bft_network_message};
pub use codec::{decode_network_message, encode_network_message};
pub use governance::{
    RemotePublicCheckpointSubmission, RemoteRecoveryCheckpointSubmission,
    RemoteValidatorTransitionSubmission, client_submit_public_checkpoint,
    client_submit_recovery_checkpoint, client_submit_validator_transition,
};
pub(crate) use governance::{
    recovery_accepted as governance_recovery_accepted, rejected as governance_rejected,
    transition_accepted as governance_transition_accepted, verify_public_checkpoint_request,
    verify_recovery_request, verify_transition_request,
};
pub use identity::{QuicTransportIdentity, transport_identity_path};
pub(crate) use peer_manager::{PeerDirection, PeerLease, PeerManager, PeerRegistrationError};
pub(crate) use peer_record::validate_peer_limit;
pub use peer_record::{
    MAX_DEPLOYED_VALIDATORS, MAX_LOCAL_PEER_CANDIDATES, MAX_PEER_CERTIFICATE_SIZE,
    MAX_PEER_RECORDS, PeerRecord,
};
pub(crate) use peer_store::PeerStore;
pub(crate) use quic::{MAX_CONCURRENT_ONE_WAY_STREAMS, outbound_bind_address};
pub use quic::{QuicClient, QuicPeer, QuicRequestStream, QuicServer, SECOND_QUIC_SERVER_NAME};
pub use recovery::{
    MAX_STATE_RECOVERY_CHUNK_SIZE, client_fetch_state_recovery, client_fetch_validator_handoff,
};
pub(crate) use recovery::{
    StateRecoveryProvider, StateRecoveryProviderHandle, new_state_recovery_provider_handle,
    state_recovery_response, validate_chunk_limit,
};
pub(crate) use session::{
    PublicNetworkServices, RuntimeNetworkSnapshot, RuntimePublicSnapshot,
    client_sync_certified_public_currency_view_from_checkpoint, serve_public_network_connection,
    serve_public_network_connection_from_request, serve_transition_proof_requests,
};
pub use session::{
    client_peer_records, client_ping, client_public_currency_checkpoint_proof,
    client_public_currency_delta, client_public_currency_page, client_public_currency_summary,
    client_sync_certified_public_currency_view, client_sync_public_currency_view,
    client_validator_set_transition_proof, serve_ping_session, serve_public_currency_connection,
};
pub use submission::{
    MAX_LEGAL_TASK_SUBMISSION_SIZE, RemoteLegalTaskSubmission, client_submit_legal_task,
};
pub(crate) use submission::{
    rejected as legal_task_submission_rejected, validate_submission_chunk, validate_submission_open,
};
pub(crate) use task_status::rejected as legal_task_status_rejected;
pub use task_status::{RemoteLegalTaskStatus, client_legal_task_status};

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LegalTaskSubmissionRejection {
    Unavailable,
    Busy,
    Rejected,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LegalTaskStatusRejection {
    Unavailable,
    Rejected,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GovernanceRejection {
    Unavailable,
    Busy,
    Unauthorized,
    Rejected,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NetworkMessage {
    AccountQuery {
        account: [u8; 32],
        kind: u8,
        cursor: u64,
        generation: Option<u64>,
        nonce: [u8; 32],
        signature: [u8; 64],
    },
    AccountQueryResult {
        view: AccountView,
    },
    AccountQueryDenied {
        reason: String,
    },
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
    GetPublicCurrencyDelta {
        from_epoch: u64,
        from_state_digest: [u8; 32],
    },
    PublicCurrencyDelta {
        delta: PublicCurrencyDelta,
    },
    NoPublicCurrencyDelta,
    GetValidatorSetTransitionProof {
        current_validator_set_version: u64,
    },
    ValidatorSetTransitionProof {
        proof: ValidatorSetTransitionProof,
    },
    NoValidatorSetTransitionProof,
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
    BftAuthenticate {
        validator_id: ValidatorId,
        validator_set_version: u64,
        signature: [u8; 64],
    },
    BftAuthenticated {
        validator_id: ValidatorId,
        validator_set_version: u64,
        signature: [u8; 64],
    },
    BftMessage {
        bytes: Vec<u8>,
    },
    LegalTaskSubmissionOpen {
        total_len: u32,
    },
    LegalTaskSubmissionChunk {
        offset: u32,
        bytes: Vec<u8>,
    },
    LegalTaskSubmissionContinue {
        next_offset: u32,
    },
    LegalTaskSubmissionAccepted {
        task_id: TaskId,
        outcome: LegalTaskSubmissionOutcome,
    },
    LegalTaskSubmissionRejected {
        reason: LegalTaskSubmissionRejection,
    },
    LegalTaskStatusQuery {
        task_id: TaskId,
        request_digest: [u8; 32],
    },
    LegalTaskStatusResult {
        task_id: TaskId,
        status: LegalTaskStatus,
    },
    LegalTaskStatusRejected {
        reason: LegalTaskStatusRejection,
    },
    ValidatorTransitionSubmit {
        validator_id: ValidatorId,
        validator_set_version: u64,
        source: Vec<u8>,
        signature: [u8; 64],
    },
    ValidatorTransitionAccepted {
        current_validator_set_version: u64,
        next_validator_set_version: u64,
        transition_digest: [u8; 32],
    },
    PublicCheckpointSubmit {
        validator_id: ValidatorId,
        validator_set_version: u64,
        signature: [u8; 64],
    },
    PublicCheckpointAccepted {
        validator_set_version: u64,
        epoch: u64,
        checkpoint_digest: [u8; 32],
    },
    StateRecoveryCheckpointSubmit {
        validator_id: ValidatorId,
        validator_set_version: u64,
        signature: [u8; 64],
    },
    StateRecoveryCheckpointAccepted {
        validator_set_version: u64,
        serial: u64,
        checkpoint_digest: [u8; 32],
    },
    GovernanceRejected {
        reason: GovernanceRejection,
    },
    BftDenied,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NetworkError {
    InvalidAccountQuery,
    AccountQueryDenied(String),
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
    InvalidLegalTaskSubmission,
    LegalTaskSubmissionRejected(LegalTaskSubmissionRejection),
    InvalidLegalTaskStatus,
    LegalTaskStatusRejected(LegalTaskStatusRejection),
    InvalidGovernanceRequest,
    GovernanceUnauthorized,
    GovernanceRejected(GovernanceRejection),
    InvalidMessageLength {
        message_type: u8,
        expected: usize,
        actual: usize,
    },
    InvalidCheckpointProof,
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
    InvalidPublicCurrencyDelta,
    InvalidValidatorTransitionProof,
    PublicStateSource(String),
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
    BftUnauthorized,
    InvalidBftMessage,
    Bft(BftError),
    ConsensusFinality(FinalityError),
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
