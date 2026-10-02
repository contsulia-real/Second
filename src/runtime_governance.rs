use crate::network::{
    GovernanceRejection, NetworkError, NetworkMessage, QuicPeer, QuicRequestStream,
    governance_recovery_accepted, governance_rejected, governance_transition_accepted,
    verify_recovery_request, verify_transition_request,
};
use crate::runtime_bft::{InboundBftMessage, ValidatorBftRuntime};
use crate::runtime_bft_consensus::{
    BftConsensusRuntimeError, start_validator_consensus_target_for,
};
use crate::runtime_consensus_target::ValidatorConsensusTarget;
use crate::{
    BftNetworkMessage, ConsensusScope, NodeRuntime, NodeRuntimeError, StateRecoveryCheckpoint,
    StateStore, ValidatorSet, ValidatorSetTransition, ValidatorSetTransitionSource,
};

#[derive(Clone)]
pub(crate) struct GovernanceContext {
    store: StateStore,
    runtime: ValidatorBftRuntime,
}

impl GovernanceContext {
    fn current(&self) -> Result<crate::PersistedNodeState, NodeRuntimeError> {
        self.store.load()?.ok_or(NodeRuntimeError::SnapshotMissing)
    }

    fn ready_for_operator_request(&self) -> Result<bool, NodeRuntimeError> {
        self.runtime.refresh_authority()?;
        Ok(self.runtime.has_all_active_validator_peers())
    }

    fn start_transition(
        &self,
        source: ValidatorSetTransitionSource,
    ) -> Result<ValidatorSetTransition, NodeRuntimeError> {
        let persisted = self.current()?;
        let transition = source
            .verify(&persisted.validator_set, &persisted.validator_registry)
            .map_err(|_| {
                NodeRuntimeError::BftConsensus(BftConsensusRuntimeError::InvalidGovernanceSource)
            })?;
        validate_bootstrap_quorum(&persisted.validator_set, transition.next_validator_set())
            .map_err(NodeRuntimeError::BftConsensus)?;
        start_validator_consensus_target_for(
            &self.store,
            &self.runtime,
            ValidatorConsensusTarget::ValidatorSetTransition(transition.clone()),
        )?;

        let source_bytes = source.encode_bytes().map_err(|error| {
            NodeRuntimeError::BftConsensus(BftConsensusRuntimeError::GovernanceSourceCodec(error))
        })?;
        let scope = ConsensusScope::ValidatorSetTransition {
            current_validator_set_version: transition.current_validator_set_version(),
        };
        let message = BftNetworkMessage::ValidatorSetTransitionSource {
            validator_set_version: transition.current_validator_set_version(),
            scope,
            bytes: source_bytes,
        };
        let failures = self.runtime.broadcast(&message);
        if !failures.is_empty() {
            self.runtime
                .consensus()
                .record_send_failures(message.scope().clone(), failures);
        }
        Ok(transition)
    }

    fn start_recovery_checkpoint(&self) -> Result<StateRecoveryCheckpoint, NodeRuntimeError> {
        let checkpoint = self.store.next_state_recovery_checkpoint()?;
        start_validator_consensus_target_for(
            &self.store,
            &self.runtime,
            ValidatorConsensusTarget::StateRecoveryCheckpoint(checkpoint.clone()),
        )?;
        let message = BftNetworkMessage::StateRecoveryCheckpointSource {
            validator_set_version: checkpoint.validator_set_version(),
            scope: ConsensusScope::StateRecoveryCheckpoint {
                validator_set_version: checkpoint.validator_set_version(),
                serial: checkpoint.serial(),
            },
            bytes: checkpoint.encode_bytes(),
        };
        let failures = self.runtime.broadcast(&message);
        if !failures.is_empty() {
            self.runtime
                .consensus()
                .record_send_failures(message.scope().clone(), failures);
        }
        Ok(checkpoint)
    }
}

impl NodeRuntime {
    pub(crate) fn governance_context(&self) -> Option<GovernanceContext> {
        let store = self.full_store().ok()?.clone();
        self.validator_bft
            .clone()
            .map(|runtime| GovernanceContext { store, runtime })
    }

    pub(crate) fn process_governance_bft_sources(
        &self,
        inbound: Vec<InboundBftMessage>,
    ) -> Vec<InboundBftMessage> {
        let Some(context) = self.governance_context() else {
            return inbound;
        };
        let mut pass = Vec::with_capacity(inbound.len());
        for envelope in inbound {
            let validator_id = envelope.validator_id;
            match envelope.message {
                BftNetworkMessage::ValidatorSetTransitionSource {
                    validator_set_version,
                    scope,
                    bytes,
                } => {
                    let result =
                        install_transition_source(&context, validator_set_version, &scope, &bytes);
                    if let Err(error) = result {
                        context.runtime.consensus().record_rejection(
                            Some(validator_id),
                            scope,
                            error,
                        );
                    }
                }
                BftNetworkMessage::StateRecoveryCheckpointSource {
                    validator_set_version,
                    scope,
                    bytes,
                } => {
                    let result =
                        install_recovery_source(&context, validator_set_version, &scope, &bytes);
                    if let Err(error) = result {
                        context.runtime.consensus().record_rejection(
                            Some(validator_id),
                            scope,
                            error,
                        );
                    }
                }
                message => pass.push(InboundBftMessage {
                    validator_id,
                    message,
                }),
            }
        }
        pass
    }
}

pub(crate) async fn serve_governance_request_from_request(
    context: GovernanceContext,
    peer: &QuicPeer,
    request: QuicRequestStream,
) -> Result<(), NetworkError> {
    if !context
        .ready_for_operator_request()
        .map_err(|_| NetworkError::InvalidGovernanceRequest)?
    {
        request
            .respond(&governance_rejected(GovernanceRejection::Busy))
            .await?;
        return Ok(());
    }

    let persisted = context
        .current()
        .map_err(|_| NetworkError::InvalidGovernanceRequest)?;
    let current_set = &persisted.validator_set;

    let response = match request.message() {
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
            } else {
                match context.start_recovery_checkpoint() {
                    Ok(checkpoint) => governance_recovery_accepted(&checkpoint),
                    Err(_) => governance_rejected(GovernanceRejection::Rejected),
                }
            }
        }
        _ => return Err(NetworkError::UnexpectedMessage),
    };
    request.respond(&response).await
}

fn install_transition_source(
    context: &GovernanceContext,
    validator_set_version: u64,
    scope: &ConsensusScope,
    bytes: &[u8],
) -> Result<(), BftConsensusRuntimeError> {
    let persisted = context
        .store
        .load()
        .map_err(BftConsensusRuntimeError::Persistence)?
        .ok_or(BftConsensusRuntimeError::InvalidGovernanceSource)?;
    if persisted.validator_set.version() != validator_set_version
        || scope
            != &(ConsensusScope::ValidatorSetTransition {
                current_validator_set_version: validator_set_version,
            })
    {
        return Err(BftConsensusRuntimeError::InvalidGovernanceSource);
    }
    let source = ValidatorSetTransitionSource::decode_bytes(bytes)
        .map_err(BftConsensusRuntimeError::GovernanceSourceCodec)?;
    let transition = source
        .verify(&persisted.validator_set, &persisted.validator_registry)
        .map_err(|_| BftConsensusRuntimeError::InvalidGovernanceSource)?;
    validate_bootstrap_quorum(&persisted.validator_set, transition.next_validator_set())?;
    start_validator_consensus_target_for(
        &context.store,
        &context.runtime,
        ValidatorConsensusTarget::ValidatorSetTransition(transition),
    )
    .map_err(node_error_to_consensus)
}

fn install_recovery_source(
    context: &GovernanceContext,
    validator_set_version: u64,
    scope: &ConsensusScope,
    bytes: &[u8; 52],
) -> Result<(), BftConsensusRuntimeError> {
    let checkpoint = StateRecoveryCheckpoint::decode_bytes(bytes)
        .ok_or(BftConsensusRuntimeError::InvalidGovernanceSource)?;
    if checkpoint.validator_set_version() != validator_set_version
        || scope
            != &(ConsensusScope::StateRecoveryCheckpoint {
                validator_set_version,
                serial: checkpoint.serial(),
            })
    {
        return Err(BftConsensusRuntimeError::InvalidGovernanceSource);
    }
    let persisted = context
        .store
        .load()
        .map_err(BftConsensusRuntimeError::Persistence)?
        .ok_or(BftConsensusRuntimeError::InvalidGovernanceSource)?;
    if persisted.validator_set.version() != validator_set_version {
        return Err(BftConsensusRuntimeError::InvalidGovernanceSource);
    }
    start_validator_consensus_target_for(
        &context.store,
        &context.runtime,
        ValidatorConsensusTarget::StateRecoveryCheckpoint(checkpoint),
    )
    .map_err(node_error_to_consensus)
}

fn validate_bootstrap_quorum(
    current: &ValidatorSet,
    next: &ValidatorSet,
) -> Result<(), BftConsensusRuntimeError> {
    let retained = next
        .credentials()
        .filter(|credential| current.contains(credential.id()))
        .count();
    if retained < next.quorum_threshold() || next.len() > usize::from(crate::MAX_PEER_RECORDS) + 1 {
        return Err(BftConsensusRuntimeError::InvalidGovernanceSource);
    }
    Ok(())
}

fn node_error_to_consensus(error: NodeRuntimeError) -> BftConsensusRuntimeError {
    match error {
        NodeRuntimeError::BftConsensus(error) => error,
        NodeRuntimeError::Persistence(error) => BftConsensusRuntimeError::Persistence(error),
        NodeRuntimeError::ValidatorBft(crate::ValidatorBftRuntimeError::ConsensusKeyMismatch(
            validator_id,
        )) => BftConsensusRuntimeError::Signing(
            crate::ValidatorSigningError::ConsensusSigningKeyMismatch(validator_id),
        ),
        _ => BftConsensusRuntimeError::InvalidGovernanceSource,
    }
}
