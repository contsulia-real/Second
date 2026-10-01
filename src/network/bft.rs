use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

use crate::{ValidatorId, ValidatorSet};

use super::bft_codec::{BftNetworkMessage, decode_bft_network_message, encode_bft_network_message};
use super::{CURRENT_NETWORK_PROTOCOL_VERSION, NetworkError, NetworkMessage, QuicPeer};

const BFT_AUTH_DOMAIN: &[u8] = b"SECOND_BFT_TRANSPORT_AUTH_V1\0";

#[derive(Clone)]
pub struct ValidatorBftPeer {
    peer: QuicPeer,
    validator_id: ValidatorId,
    validator_set: ValidatorSet,
}

impl ValidatorBftPeer {
    pub const fn validator_id(&self) -> ValidatorId {
        self.validator_id
    }

    pub fn remote_node_id(&self) -> super::NodeId {
        self.peer.remote_node_id()
    }

    pub async fn send(&self, message: &BftNetworkMessage) -> Result<(), NetworkError> {
        verify_bft_sender(message, self.validator_id, &self.validator_set)?;
        let bytes = encode_bft_network_message(message)?;
        match self
            .peer
            .exchange(&NetworkMessage::BftMessage { bytes })
            .await?
        {
            NetworkMessage::BftAccepted => Ok(()),
            NetworkMessage::BftDenied => Err(NetworkError::BftUnauthorized),
            _ => Err(NetworkError::UnexpectedMessage),
        }
    }

    pub fn close(&self) {
        self.peer.close();
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
        .sign(&bft_auth_bytes(binding, validator_id))
        .to_bytes();
    match peer
        .exchange(&NetworkMessage::BftAuthenticate {
            validator_id,
            signature,
        })
        .await?
    {
        NetworkMessage::BftAuthenticated => Ok(ValidatorBftPeer {
            peer,
            validator_id,
            validator_set: validator_set.clone(),
        }),
        NetworkMessage::BftDenied => Err(NetworkError::BftUnauthorized),
        _ => Err(NetworkError::UnexpectedMessage),
    }
}

pub async fn serve_validator_bft_connection<F>(
    peer: &QuicPeer,
    validator_set: &ValidatorSet,
    mut on_message: F,
) -> Result<ValidatorId, NetworkError>
where
    F: FnMut(ValidatorId, BftNetworkMessage) -> Result<(), NetworkError>,
{
    let Some(auth_request) = peer.accept_request().await? else {
        return Err(NetworkError::BftUnauthorized);
    };
    let validator_id = match auth_request.message() {
        NetworkMessage::BftAuthenticate {
            validator_id,
            signature,
        } if verify_bft_auth(peer, *validator_id, *signature, validator_set)? => {
            let validator_id = *validator_id;
            auth_request
                .respond(&NetworkMessage::BftAuthenticated)
                .await?;
            validator_id
        }
        _ => {
            auth_request.respond(&NetworkMessage::BftDenied).await?;
            return Err(NetworkError::BftUnauthorized);
        }
    };

    loop {
        let Some(request) = peer.accept_request().await? else {
            return Ok(validator_id);
        };
        let NetworkMessage::BftMessage { bytes } = request.message() else {
            request.respond(&NetworkMessage::BftDenied).await?;
            return Err(NetworkError::BftUnauthorized);
        };

        let message = match decode_bft_network_message(bytes) {
            Ok(message) => message,
            Err(error) => {
                request.respond(&NetworkMessage::BftDenied).await?;
                return Err(error);
            }
        };
        if let Err(error) = verify_bft_sender(&message, validator_id, validator_set) {
            request.respond(&NetworkMessage::BftDenied).await?;
            return Err(error);
        }
        if let Err(error) = on_message(validator_id, message) {
            request.respond(&NetworkMessage::BftDenied).await?;
            return Err(error);
        }
        request.respond(&NetworkMessage::BftAccepted).await?;
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
    }
    Ok(())
}

fn verify_bft_auth(
    peer: &QuicPeer,
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
            &bft_auth_bytes(binding, validator_id),
            &Signature::from_bytes(&signature),
        )
        .is_ok())
}

fn bft_auth_bytes(binding: [u8; 32], validator_id: ValidatorId) -> Vec<u8> {
    let mut out = Vec::with_capacity(BFT_AUTH_DOMAIN.len() + 4 + 32 + 8);
    out.extend_from_slice(BFT_AUTH_DOMAIN);
    out.extend_from_slice(&CURRENT_NETWORK_PROTOCOL_VERSION.to_be_bytes());
    out.extend_from_slice(&binding);
    out.extend_from_slice(&validator_id.value().to_be_bytes());
    out
}
