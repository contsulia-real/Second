use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

use crate::{ValidatorId, ValidatorSet};

use super::bft_codec::{BftNetworkMessage, decode_bft_network_message, encode_bft_network_message};
use super::quic::MAX_CONCURRENT_ONE_WAY_STREAMS;
use super::{CURRENT_NETWORK_PROTOCOL_VERSION, NetworkError, NetworkMessage, QuicPeer};

const BFT_AUTH_DOMAIN: &[u8] = b"SECOND_BFT_TRANSPORT_AUTH_V1\0";

#[derive(Clone, Copy)]
enum BftAuthRole {
    Client = 1,
    Server = 2,
}

#[derive(Clone)]
pub struct ValidatorBftPeer {
    peer: QuicPeer,
    local_validator_id: ValidatorId,
    remote_validator_id: ValidatorId,
    validator_set: ValidatorSet,
}

impl ValidatorBftPeer {
    pub const fn local_validator_id(&self) -> ValidatorId {
        self.local_validator_id
    }

    pub const fn remote_validator_id(&self) -> ValidatorId {
        self.remote_validator_id
    }

    pub fn remote_node_id(&self) -> super::NodeId {
        self.peer.remote_node_id()
    }

    pub async fn send(&self, message: &BftNetworkMessage) -> Result<(), NetworkError> {
        verify_bft_sender(message, self.local_validator_id, &self.validator_set)?;
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
    let credential = validator_set
        .validator(validator_id)
        .ok_or(NetworkError::BftUnauthorized)?;
    if credential.identity_public_key() != identity_key.verifying_key().to_bytes() {
        return Err(NetworkError::BftUnauthorized);
    }

    let binding = peer.channel_binding()?;
    let signature = identity_key
        .sign(&bft_auth_bytes(binding, BftAuthRole::Client, validator_id))
        .to_bytes();
    match peer
        .exchange(&NetworkMessage::BftAuthenticate {
            validator_id,
            signature,
        })
        .await?
    {
        NetworkMessage::BftAuthenticated {
            validator_id: remote_validator_id,
            signature,
        } => {
            if !verify_bft_auth(
                &peer,
                BftAuthRole::Server,
                remote_validator_id,
                signature,
                validator_set,
            )? {
                return Err(NetworkError::BftUnauthorized);
            }
            Ok(ValidatorBftPeer {
                peer,
                local_validator_id: validator_id,
                remote_validator_id,
                validator_set: validator_set.clone(),
            })
        }
        NetworkMessage::BftDenied => Err(NetworkError::BftUnauthorized),
        _ => Err(NetworkError::UnexpectedMessage),
    }
}

pub async fn serve_validator_bft_connection<F>(
    peer: &QuicPeer,
    local_validator_id: ValidatorId,
    local_identity_key: &SigningKey,
    validator_set: &ValidatorSet,
    on_message: F,
) -> Result<ValidatorId, NetworkError>
where
    F: FnMut(ValidatorId, BftNetworkMessage) -> Result<(), NetworkError>,
{
    let Some(auth_request) = peer.accept_request().await? else {
        return Err(NetworkError::BftUnauthorized);
    };
    serve_validator_bft_connection_from_request(
        peer,
        auth_request,
        local_validator_id,
        local_identity_key,
        validator_set,
        on_message,
    )
    .await
}

pub(crate) async fn serve_validator_bft_connection_from_request<F>(
    peer: &QuicPeer,
    auth_request: super::QuicRequestStream,
    local_validator_id: ValidatorId,
    local_identity_key: &SigningKey,
    validator_set: &ValidatorSet,
    mut on_message: F,
) -> Result<ValidatorId, NetworkError>
where
    F: FnMut(ValidatorId, BftNetworkMessage) -> Result<(), NetworkError>,
{
    let validator_id = match auth_request.message() {
        NetworkMessage::BftAuthenticate {
            validator_id,
            signature,
        } if verify_bft_auth(
            peer,
            BftAuthRole::Client,
            *validator_id,
            *signature,
            validator_set,
        )? =>
        {
            let validator_id = *validator_id;
            let local_credential = validator_set
                .validator(local_validator_id)
                .ok_or(NetworkError::BftUnauthorized)?;
            if local_credential.identity_public_key()
                != local_identity_key.verifying_key().to_bytes()
            {
                auth_request.respond(&NetworkMessage::BftDenied).await?;
                return Err(NetworkError::BftUnauthorized);
            }
            let binding = peer.channel_binding()?;
            let signature = local_identity_key
                .sign(&bft_auth_bytes(
                    binding,
                    BftAuthRole::Server,
                    local_validator_id,
                ))
                .to_bytes();
            auth_request
                .respond_without_delivery_wait(&NetworkMessage::BftAuthenticated {
                    validator_id: local_validator_id,
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
        verify_bft_sender(&message, validator_id, validator_set)?;
        on_message(validator_id, message)?;
    }
}

fn verify_bft_sender(
    message: &BftNetworkMessage,
    sender: ValidatorId,
    validator_set: &ValidatorSet,
) -> Result<(), NetworkError> {
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
    }
    Ok(())
}

fn verify_bft_auth(
    peer: &QuicPeer,
    role: BftAuthRole,
    validator_id: ValidatorId,
    signature: [u8; 64],
    validator_set: &ValidatorSet,
) -> Result<bool, NetworkError> {
    let Some(credential) = validator_set.validator(validator_id) else {
        return Ok(false);
    };
    let key = VerifyingKey::from_bytes(&credential.identity_public_key())
        .map_err(|_| NetworkError::BftUnauthorized)?;
    let binding = peer.channel_binding()?;
    Ok(key
        .verify_strict(
            &bft_auth_bytes(binding, role, validator_id),
            &Signature::from_bytes(&signature),
        )
        .is_ok())
}

fn bft_auth_bytes(binding: [u8; 32], role: BftAuthRole, validator_id: ValidatorId) -> Vec<u8> {
    let mut out = Vec::with_capacity(BFT_AUTH_DOMAIN.len() + 4 + 32 + 1 + 8);
    out.extend_from_slice(BFT_AUTH_DOMAIN);
    out.extend_from_slice(&CURRENT_NETWORK_PROTOCOL_VERSION.to_be_bytes());
    out.extend_from_slice(&binding);
    out.push(role as u8);
    out.extend_from_slice(&validator_id.value().to_be_bytes());
    out
}
