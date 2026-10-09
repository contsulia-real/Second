//! Authenticated operator requests and blocking governance transactions.
use super::*;

pub(crate) async fn serve_governance_request_from_request(
    context: GovernanceContext,
    peer: &QuicPeer,
    request: QuicRequestStream,
    connection_permit: crate::runtime::ActiveConnectionPermit,
) -> Result<(), NetworkError> {
    let peer = peer.clone();
    let (response, request, _permit) = tokio::task::spawn_blocking(move || {
        let response = governance_response(&context, &peer, request.message());
        (response, request, connection_permit)
    })
    .await
    .map_err(|error| NetworkError::Transport(format!("governance worker failed: {error}")))?;
    request.respond(&response?).await
}

fn governance_response(
    context: &GovernanceContext,
    peer: &QuicPeer,
    message: &NetworkMessage,
) -> Result<NetworkMessage, NetworkError> {
    let ready = context
        .ready_for_operator_request()
        .map_err(|_| NetworkError::InvalidGovernanceRequest)?;

    let persisted = context
        .current()
        .map_err(|_| NetworkError::InvalidGovernanceRequest)?;
    let current_set = &persisted.validator_set;

    let response = match message {
        NetworkMessage::ValidatorTransitionSubmit {
            validator_id,
            validator_set_version,
            source,
            signature,
        } => {
            if verify_transition_request(
                peer,
                *validator_id,
                *validator_set_version,
                source,
                *signature,
                current_set,
            )
            .is_err()
            {
                governance_rejected(GovernanceRejection::Unauthorized)
            } else if !ready {
                governance_rejected(GovernanceRejection::Busy)
            } else {
                match ValidatorSetTransitionSource::decode_bytes(source)
                    .map_err(|_| ())
                    .and_then(|source| context.start_transition(source).map_err(|_| ()))
                {
                    Ok(transition) => governance_transition_accepted(&transition),
                    Err(()) => governance_rejected(GovernanceRejection::Rejected),
                }
            }
        }
        NetworkMessage::PublicCheckpointSubmit {
            validator_id,
            validator_set_version,
            signature,
        } => {
            if verify_public_checkpoint_request(
                peer,
                *validator_id,
                *validator_set_version,
                *signature,
                current_set,
            )
            .is_err()
            {
                governance_rejected(GovernanceRejection::Unauthorized)
            } else if !ready {
                governance_rejected(GovernanceRejection::Busy)
            } else {
                match context.start_public_checkpoint() {
                    Ok(checkpoint) => NetworkMessage::PublicCheckpointAccepted {
                        validator_set_version: current_set.version(),
                        epoch: checkpoint.epoch(),
                        checkpoint_digest: checkpoint.digest(),
                    },
                    Err(_) => governance_rejected(GovernanceRejection::Rejected),
                }
            }
        }
        NetworkMessage::StateRecoveryCheckpointSubmit {
            validator_id,
            validator_set_version,
            signature,
        } => {
            if verify_recovery_request(
                peer,
                *validator_id,
                *validator_set_version,
                *signature,
                current_set,
            )
            .is_err()
            {
                governance_rejected(GovernanceRejection::Unauthorized)
            } else if !ready {
                governance_rejected(GovernanceRejection::Busy)
            } else {
                match context.start_recovery_checkpoint() {
                    Ok(checkpoint) => governance_recovery_accepted(&checkpoint),
                    Err(_) => governance_rejected(GovernanceRejection::Rejected),
                }
            }
        }
        _ => return Err(NetworkError::UnexpectedMessage),
    };
    Ok(response)
}
