use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::sync::{Arc, RwLock};

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

use crate::{ConsensusScope, ValidatorId, ValidatorSet};

use super::bft_codec::{BftNetworkMessage, decode_bft_network_message, encode_bft_network_message};
use super::quic::MAX_CONCURRENT_ONE_WAY_STREAMS;
use super::{CURRENT_NETWORK_PROTOCOL_VERSION, NetworkError, NetworkMessage, QuicPeer};

const BFT_AUTH_DOMAIN: &[u8] = b"SECOND_BFT_TRANSPORT_AUTH_V1\0";

#[derive(Clone, Copy)]
enum BftAuthRole {
    Client = 1,
    Server = 2,
}

pub(crate) type SharedValidatorBftAuthority = Arc<RwLock<ValidatorBftAuthority>>;

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct ValidatorBftAuthority {
    active_validator_set_version: u64,
    validator_sets: BTreeMap<u64, ValidatorSet>,
    // Derived terminal-proof audiences, including certified inherited duties.
    terminal_scopes: BTreeMap<ConsensusScope, ValidatorSet>,
    live_validator_ids: BTreeSet<ValidatorId>,
    identity_public_keys: BTreeMap<ValidatorId, [u8; 32]>,
}

impl ValidatorBftAuthority {
    pub(crate) fn new(
        active_validator_set: ValidatorSet,
        retained_validator_sets: impl IntoIterator<Item = ValidatorSet>,
    ) -> Result<Self, NetworkError> {
        Self::new_with_terminal_scopes(
            active_validator_set,
            retained_validator_sets,
            std::iter::empty(),
        )
    }

    pub(crate) fn new_with_terminal_scopes(
        active_validator_set: ValidatorSet,
        retained_validator_sets: impl IntoIterator<Item = ValidatorSet>,
        terminal_scopes: impl IntoIterator<Item = (ConsensusScope, ValidatorSet)>,
    ) -> Result<Self, NetworkError> {
        let active_validator_set_version = active_validator_set.version();
        let mut validator_sets = BTreeMap::new();
        validator_sets.insert(active_validator_set_version, active_validator_set);
        for validator_set in retained_validator_sets {
            if validator_sets
                .insert(validator_set.version(), validator_set)
                .is_some()
            {
                return Err(NetworkError::BftUnauthorized);
            }
        }

        let mut completed = BTreeMap::new();
        for (scope, validator_set) in terminal_scopes {
            if !scope.matches_validator_set_version(validator_set.version())
                || completed.insert(scope, validator_set).is_some()
            {
                return Err(NetworkError::BftUnauthorized);
            }
        }

        let live_validator_ids = validator_sets
            .values()
            .flat_map(ValidatorSet::credentials)
            .map(|credential| credential.id())
            .collect::<BTreeSet<_>>();

        let mut identity_public_keys = BTreeMap::new();
        for validator_set in validator_sets.values().chain(completed.values()) {
            for credential in validator_set.credentials() {
                match identity_public_keys.insert(credential.id(), credential.identity_public_key())
                {
                    Some(existing) if existing != credential.identity_public_key() => {
                        return Err(NetworkError::BftUnauthorized);
                    }
                    _ => {}
                }
            }
        }

        Ok(Self {
            active_validator_set_version,
            validator_sets,
            terminal_scopes: completed,
            live_validator_ids,
            identity_public_keys,
        })
    }

    fn active_only(validator_set: &ValidatorSet) -> Self {
        Self::new(validator_set.clone(), std::iter::empty())
            .expect("one validated ValidatorSet must form a valid BFT authority")
    }

    pub(crate) fn active_validator_set(&self) -> &ValidatorSet {
        self.validator_sets
            .get(&self.active_validator_set_version)
            .expect("active ValidatorSet is always retained by BFT authority")
    }

    pub(crate) fn validator_ids(&self) -> impl Iterator<Item = ValidatorId> + '_ {
        self.live_validator_ids.iter().copied()
    }

    pub(crate) fn identity_public_key(&self, validator_id: ValidatorId) -> Option<[u8; 32]> {
        self.identity_public_keys.get(&validator_id).copied()
    }

    fn validator_set_for_message(
        &self,
        message: &BftNetworkMessage,
    ) -> Result<&ValidatorSet, NetworkError> {
        let version = message.validator_set_version();
        if matches!(message.scope(), ConsensusScope::PreparedTask(_))
            && let Some(validator_set) = self.validator_sets.get(&version)
        {
            return Ok(validator_set);
        }
        if version == self.active_validator_set_version {
            return self
                .validator_sets
                .get(&version)
                .ok_or(NetworkError::BftUnauthorized);
        }
        self.terminal_scopes
            .get(message.scope())
            .filter(|validator_set| validator_set.version() == version)
            .ok_or(NetworkError::BftUnauthorized)
    }

    fn accepts_recipient(
        &self,
        recipient: ValidatorId,
        message: &BftNetworkMessage,
    ) -> Result<bool, NetworkError> {
        let origin = self.validator_set_for_message(message)?;
        Ok(origin.contains(recipient)
            || (matches!(
                message,
                BftNetworkMessage::FinalityCertificate {
                    scope: ConsensusScope::PreparedTask(_),
                    ..
                }
            ) && self.terminal_scopes.get(message.scope()) == Some(origin)
                && self.active_validator_set().contains(recipient)))
    }
}

#[derive(Clone)]
pub struct ValidatorBftPeer {
    peer: QuicPeer,
    local_validator_id: ValidatorId,
    remote_validator_id: ValidatorId,
    remote_validator_set_version: u64,
    authority: SharedValidatorBftAuthority,
}

impl ValidatorBftPeer {
    pub(crate) fn remote_validator_set_version(&self) -> u64 {
        self.remote_validator_set_version
    }
    pub const fn local_validator_id(&self) -> ValidatorId {
        self.local_validator_id
    }

    pub const fn remote_validator_id(&self) -> ValidatorId {
        self.remote_validator_id
    }

    pub fn remote_node_id(&self) -> super::NodeId {
        self.peer.remote_node_id()
    }

    pub(crate) fn can_send(&self, message: &BftNetworkMessage) -> bool {
        let authority = self
            .authority
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        authority
            .validator_set_for_message(message)
            .is_ok_and(|origin| origin.contains(self.local_validator_id))
            && authority
                .accepts_recipient(self.remote_validator_id, message)
                .unwrap_or(false)
    }

    pub async fn send(&self, message: &BftNetworkMessage) -> Result<(), NetworkError> {
        {
            let authority = self
                .authority
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !authority.accepts_recipient(self.remote_validator_id, message)? {
                return Err(NetworkError::BftUnauthorized);
            }
            verify_bft_sender(message, self.local_validator_id, &authority)?;
        }
        let bytes = encode_bft_network_message(message)?;
        self.peer
            .send_one_way(&NetworkMessage::BftMessage { bytes })
            .await
    }

    pub fn close(&self) {
        self.peer.close();
    }

    pub async fn shutdown(&self) -> Result<(), NetworkError> {
        self.peer.close();
        Ok(())
    }
}

pub async fn authenticate_validator_bft_peer(
    peer: QuicPeer,
    validator_id: ValidatorId,
    identity_key: &SigningKey,
    validator_set: &ValidatorSet,
) -> Result<ValidatorBftPeer, NetworkError> {
    let authority = Arc::new(RwLock::new(ValidatorBftAuthority::active_only(
        validator_set,
    )));
    authenticate_validator_bft_peer_with_authority(peer, validator_id, identity_key, &authority)
        .await
}

pub(crate) async fn authenticate_validator_bft_peer_with_authority(
    peer: QuicPeer,
    validator_id: ValidatorId,
    identity_key: &SigningKey,
    authority: &SharedValidatorBftAuthority,
) -> Result<ValidatorBftPeer, NetworkError> {
    if authority
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .identity_public_key(validator_id)
        != Some(identity_key.verifying_key().to_bytes())
    {
        return Err(NetworkError::BftUnauthorized);
    }

    let validator_set_version = authority
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .active_validator_set_version;
    let binding = peer.channel_binding()?;
    let signature = identity_key
        .sign(&bft_auth_bytes(
            binding,
            BftAuthRole::Client,
            validator_id,
            validator_set_version,
        ))
        .to_bytes();
    match peer
        .exchange(&NetworkMessage::BftAuthenticate {
            validator_id,
            validator_set_version,
            signature,
        })
        .await?
    {
        NetworkMessage::BftAuthenticated {
            validator_id: remote_validator_id,
            validator_set_version: remote_validator_set_version,
            signature,
        } => {
            if !verify_bft_auth_shared(
                &peer,
                BftAuthRole::Server,
                remote_validator_id,
                remote_validator_set_version,
                signature,
                authority,
            )? {
                return Err(NetworkError::BftUnauthorized);
            }
            Ok(ValidatorBftPeer {
                peer,
                local_validator_id: validator_id,
                remote_validator_id,
                remote_validator_set_version,
                authority: Arc::clone(authority),
            })
        }
        NetworkMessage::BftDenied => Err(NetworkError::BftUnauthorized),
        _ => Err(NetworkError::UnexpectedMessage),
    }
}

pub async fn serve_validator_bft_connection<F, Fut>(
    peer: &QuicPeer,
    local_validator_id: ValidatorId,
    local_identity_key: &SigningKey,
    validator_set: &ValidatorSet,
    on_message: F,
) -> Result<ValidatorId, NetworkError>
where
    F: FnMut(ValidatorId, BftNetworkMessage) -> Fut,
    Fut: Future<Output = Result<(), NetworkError>>,
{
    let Some(auth_request) = peer.accept_request().await? else {
        return Err(NetworkError::BftUnauthorized);
    };
    let authority = Arc::new(RwLock::new(ValidatorBftAuthority::active_only(
        validator_set,
    )));
    serve_validator_bft_connection_from_request(
        peer,
        auth_request,
        local_validator_id,
        local_identity_key,
        &authority,
        on_message,
    )
    .await
}

pub(crate) async fn serve_validator_bft_connection_from_request<F, Fut>(
    peer: &QuicPeer,
    auth_request: super::QuicRequestStream,
    local_validator_id: ValidatorId,
    local_identity_key: &SigningKey,
    authority: &SharedValidatorBftAuthority,
    mut on_message: F,
) -> Result<ValidatorId, NetworkError>
where
    F: FnMut(ValidatorId, BftNetworkMessage) -> Fut,
    Fut: Future<Output = Result<(), NetworkError>>,
{
    // Pin only the trusted handshake committee, for validating and discarding
    // control messages already in flight when local membership changes.
    let authenticated_validator_set = match auth_request.message() {
        NetworkMessage::BftAuthenticate {
            validator_set_version,
            ..
        } => authority
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .validator_sets
            .get(validator_set_version)
            .cloned(),
        _ => None,
    };
    let validator_id = match auth_request.message() {
        NetworkMessage::BftAuthenticate {
            validator_id,
            validator_set_version,
            signature,
        } if verify_bft_auth_shared(
            peer,
            BftAuthRole::Client,
            *validator_id,
            *validator_set_version,
            *signature,
            authority,
        )? =>
        {
            let validator_id = *validator_id;
            if authority
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .identity_public_key(local_validator_id)
                != Some(local_identity_key.verifying_key().to_bytes())
            {
                auth_request.respond(&NetworkMessage::BftDenied).await?;
                return Err(NetworkError::BftUnauthorized);
            }
            let validator_set_version = authority
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .active_validator_set_version;
            let binding = peer.channel_binding()?;
            let signature = local_identity_key
                .sign(&bft_auth_bytes(
                    binding,
                    BftAuthRole::Server,
                    local_validator_id,
                    validator_set_version,
                ))
                .to_bytes();
            auth_request
                .respond_without_delivery_wait(&NetworkMessage::BftAuthenticated {
                    validator_id: local_validator_id,
                    validator_set_version,
                    signature,
                })
                .await?;
            validator_id
        }
        _ => {
            auth_request.respond(&NetworkMessage::BftDenied).await?;
            return Err(NetworkError::BftUnauthorized);
        }
    };

    let mut accepts = tokio::task::JoinSet::new();
    for _ in 0..MAX_CONCURRENT_ONE_WAY_STREAMS {
        let accept_peer = peer.clone();
        accepts.spawn(async move { accept_peer.accept_one_way().await });
    }

    loop {
        let result = accepts.join_next().await;
        let network_message = match result {
            Some(Ok(Ok(Some(message)))) => message,
            Some(Ok(Ok(None))) | None => {
                accepts.abort_all();
                return Ok(validator_id);
            }
            Some(Ok(Err(error))) => {
                accepts.abort_all();
                return Err(error);
            }
            Some(Err(error)) => {
                accepts.abort_all();
                return Err(NetworkError::Transport(format!(
                    "BFT receive task failed: {error}"
                )));
            }
        };

        let accept_peer = peer.clone();
        accepts.spawn(async move { accept_peer.accept_one_way().await });

        let NetworkMessage::BftMessage { bytes } = network_message else {
            accepts.abort_all();
            return Err(NetworkError::BftUnauthorized);
        };

        let message = decode_bft_network_message(&bytes)?;
        {
            let authority = authority
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if message.validator_set_version() < authority.active_validator_set_version
                && !matches!(message.scope(), ConsensusScope::PreparedTask(_))
                && authority.validator_set_for_message(&message).is_err()
                && let Some(origin) = authenticated_validator_set
                    .as_ref()
                    .filter(|origin| origin.version() == message.validator_set_version())
            {
                if !origin.contains(local_validator_id) {
                    return Err(NetworkError::BftUnauthorized);
                }
                verify_bft_sender_for_set(&message, validator_id, origin)?;
                continue;
            }
            if !authority.accepts_recipient(local_validator_id, &message)? {
                return Err(NetworkError::BftUnauthorized);
            }
            verify_bft_sender(&message, validator_id, &authority)?;
        }
        on_message(validator_id, message).await?;
    }
}

fn verify_bft_sender(
    message: &BftNetworkMessage,
    sender: ValidatorId,
    authority: &ValidatorBftAuthority,
) -> Result<(), NetworkError> {
    let validator_set = authority.validator_set_for_message(message)?;
    verify_bft_sender_for_set(message, sender, validator_set)
}

fn verify_bft_sender_for_set(
    message: &BftNetworkMessage,
    sender: ValidatorId,
    validator_set: &ValidatorSet,
) -> Result<(), NetworkError> {
    if !validator_set.contains(sender) {
        return Err(NetworkError::BftUnauthorized);
    }

    match message {
        BftNetworkMessage::Proposal {
            proposal,
            unlock_certificate,
        } => {
            if proposal.proposer_id() != sender {
                return Err(NetworkError::BftUnauthorized);
            }
            proposal.verify(validator_set).map_err(NetworkError::Bft)?;
            if let Some(certificate) = unlock_certificate {
                certificate
                    .verify(validator_set)
                    .map_err(NetworkError::Bft)?;
            }
        }
        BftNetworkMessage::Vote { statement, vote } => {
            if vote.validator_id() != sender {
                return Err(NetworkError::BftUnauthorized);
            }
            vote.verify(statement, validator_set)
                .map_err(NetworkError::Bft)?;
        }
        BftNetworkMessage::QuorumCertificate(certificate) => {
            certificate
                .verify(validator_set)
                .map_err(NetworkError::Bft)?;
        }
        BftNetworkMessage::FinalityVote {
            scope,
            statement,
            vote,
        } => {
            if !scope.matches_validator_set_version(statement.validator_set_version()) {
                return Err(NetworkError::BftUnauthorized);
            }
            vote.verify(statement, validator_set)
                .map_err(NetworkError::ConsensusFinality)?;
        }
        BftNetworkMessage::FinalityCertificate { scope, certificate } => {
            if !scope.matches_validator_set_version(certificate.statement().validator_set_version())
            {
                return Err(NetworkError::BftUnauthorized);
            }
            certificate
                .verify(validator_set)
                .map_err(NetworkError::ConsensusFinality)?;
        }
        BftNetworkMessage::PreparedTaskRequest { scope, .. }
        | BftNetworkMessage::PreparedTaskSourceChunk { scope, .. }
        | BftNetworkMessage::PreparedTaskSourceUnavailable { scope, .. }
        | BftNetworkMessage::PreparedTaskAvailable { scope, .. } => {
            if !matches!(
                scope,
                ConsensusScope::PreparedTask(_) | ConsensusScope::CurrencyAllocation { .. }
            ) {
                return Err(NetworkError::BftUnauthorized);
            }
        }
        BftNetworkMessage::ValidatorSetTransitionSource { scope, .. } => {
            if !matches!(scope, ConsensusScope::CurrencyAllocation { .. }) {
                return Err(NetworkError::BftUnauthorized);
            }
        }
        BftNetworkMessage::PublicCheckpointSource { scope, .. } => {
            if !matches!(scope, ConsensusScope::PublicCheckpoint { .. }) {
                return Err(NetworkError::BftUnauthorized);
            }
        }
        BftNetworkMessage::StateRecoveryCheckpointSource { scope, .. } => {
            if !matches!(scope, ConsensusScope::StateRecoveryCheckpoint { .. }) {
                return Err(NetworkError::BftUnauthorized);
            }
        }
    }
    Ok(())
}

fn verify_bft_auth_shared(
    peer: &QuicPeer,
    role: BftAuthRole,
    validator_id: ValidatorId,
    validator_set_version: u64,
    signature: [u8; 64],
    authority: &SharedValidatorBftAuthority,
) -> Result<bool, NetworkError> {
    let authority = authority
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    verify_bft_auth(
        peer,
        role,
        validator_id,
        validator_set_version,
        signature,
        &authority,
    )
}

fn verify_bft_auth(
    peer: &QuicPeer,
    role: BftAuthRole,
    validator_id: ValidatorId,
    validator_set_version: u64,
    signature: [u8; 64],
    authority: &ValidatorBftAuthority,
) -> Result<bool, NetworkError> {
    let Some(identity_public_key) = authority.identity_public_key(validator_id) else {
        return Ok(false);
    };
    let key = VerifyingKey::from_bytes(&identity_public_key)
        .map_err(|_| NetworkError::BftUnauthorized)?;
    let binding = peer.channel_binding()?;
    Ok(key
        .verify_strict(
            &bft_auth_bytes(binding, role, validator_id, validator_set_version),
            &Signature::from_bytes(&signature),
        )
        .is_ok())
}

fn bft_auth_bytes(
    binding: [u8; 32],
    role: BftAuthRole,
    validator_id: ValidatorId,
    validator_set_version: u64,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(BFT_AUTH_DOMAIN.len() + 4 + 32 + 1 + 8 + 8);
    out.extend_from_slice(BFT_AUTH_DOMAIN);
    out.extend_from_slice(&CURRENT_NETWORK_PROTOCOL_VERSION.to_be_bytes());
    out.extend_from_slice(&binding);
    out.push(role as u8);
    out.extend_from_slice(&validator_id.value().to_be_bytes());
    out.extend_from_slice(&validator_set_version.to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authenticated_membership_version_cannot_be_changed_without_resigning() {
        let key = SigningKey::from_bytes(&[17; 32]);
        let binding = [9; 32];
        let id = ValidatorId::new(2);
        let original = bft_auth_bytes(binding, BftAuthRole::Server, id, 7);
        let signature = key.sign(&original);
        let public = key.verifying_key();
        assert!(public.verify_strict(&original, &signature).is_ok());
        assert!(
            public
                .verify_strict(
                    &bft_auth_bytes(binding, BftAuthRole::Server, id, 8),
                    &signature
                )
                .is_err()
        );
    }
}
