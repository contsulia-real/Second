mod requests;
pub(crate) use requests::serve_governance_request_from_request;

use crate::network::{
    GovernanceRejection, NetworkError, NetworkMessage, QuicPeer, QuicRequestStream,
    governance_recovery_accepted, governance_rejected, governance_transition_accepted,
    verify_public_checkpoint_request, verify_recovery_request, verify_transition_request,
};
use crate::runtime_bft::{InboundBftMessage, ValidatorBftRuntime};
use crate::runtime_bft_consensus::{
    BftConsensusRuntimeError, start_validator_consensus_target_for,
};
use crate::runtime_consensus_target::ValidatorConsensusTarget;
use crate::{
    BftNetworkMessage, ConsensusScope, NodeRuntime, NodeRuntimeError, PersistenceError,
    StateRecoveryCheckpoint, StateStore, ValidatorSet, ValidatorSetTransition,
    ValidatorSetTransitionSource,
};

mod collection;
pub(crate) mod handoff_source;

#[derive(Clone)]
pub(crate) struct GovernanceContext {
    store: StateStore,
    runtime: ValidatorBftRuntime,
}

impl GovernanceContext {
    fn current(&self) -> Result<std::sync::Arc<crate::PersistedNodeState>, NodeRuntimeError> {
        self.store
            .load_shared()?
            .ok_or(NodeRuntimeError::SnapshotMissing)
    }

    fn ready_for_operator_request(&self) -> Result<bool, NodeRuntimeError> {
        self.runtime.refresh_authority()?;
        let validators = self.runtime.validator_set();
        let connected = self
            .runtime
            .connected_validator_ids()
            .into_iter()
            .filter(|id| validators.contains(*id) && *id != self.runtime.validator_id())
            .count();
        Ok(
            connected + usize::from(validators.contains(self.runtime.validator_id()))
                >= validators.quorum_threshold(),
        )
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
        let transition = self.store.prepare_validator_set_transition(transition)?;
        validate_bootstrap_quorum(&persisted.validator_set, transition.next_validator_set())
            .map_err(NodeRuntimeError::BftConsensus)?;
        self.begin_transition(&transition)
    }

    fn start_public_checkpoint(&self) -> Result<crate::PublicCurrencyCheckpoint, NodeRuntimeError> {
        let checkpoint = self
            .runtime
            .consensus()
            .public_checkpoint_candidate(&self.store)?;
        let current = self.current()?;
        let version = current.validator_set.version();
        let cached = current
            .public_checkpoint_proof
            .as_ref()
            .filter(|proof| proof.checkpoint() == &checkpoint);
        if cached.is_none() {
            start_validator_consensus_target_for(
                &self.store,
                &self.runtime,
                ValidatorConsensusTarget::PublicCheckpoint(checkpoint.clone()),
            )?;
        }
        let scope = ConsensusScope::PublicCheckpoint {
            validator_set_version: version,
            epoch: checkpoint.epoch(),
        };
        let proof = cached.or(current.public_checkpoint_baseline.as_ref());
        if let Some(proof) = proof.filter(|proof| proof.validator_set_version() == version) {
            let bytes = proof.encode_bytes().map_err(|_| {
                NodeRuntimeError::BftConsensus(BftConsensusRuntimeError::InvalidGovernanceSource)
            })?;
            let message = BftNetworkMessage::PublicCheckpointSource {
                validator_set_version: version,
                scope: ConsensusScope::PublicCheckpoint {
                    validator_set_version: version,
                    epoch: proof.checkpoint().epoch(),
                },
                bytes,
            };
            let failures = self.runtime.broadcast(&message);
            if !failures.is_empty() {
                self.runtime
                    .consensus()
                    .record_send_failures(message.scope().clone(), failures);
            }
        }
        if cached.is_none() {
            let message = BftNetworkMessage::PublicCheckpointSource {
                validator_set_version: version,
                scope,
                bytes: checkpoint.encode_source(version),
            };
            let failures = self.runtime.broadcast(&message);
            if !failures.is_empty() {
                self.runtime
                    .consensus()
                    .record_send_failures(message.scope().clone(), failures);
            }
        }
        Ok(checkpoint)
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
            bytes: checkpoint.encode_source()?,
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
                BftNetworkMessage::FinalityCertificate { scope, certificate }
                    if matches!(scope, ConsensusScope::CurrencyAllocation { .. }) =>
                {
                    let installed = context.current().and_then(|snapshot| {
                        if scope
                            != (ConsensusScope::CurrencyAllocation {
                                validator_set_version: snapshot.validator_set.version(),
                                start: snapshot.state.next_currency_address(),
                            })
                        {
                            // Older allocation/transition relay still belongs to
                            // the existing completed-scope proof path.
                            return Ok(false);
                        }
                        let transition = snapshot
                            .pending_governance
                            .get(&certificate.statement().subject_digest())
                            .and_then(crate::persistence::PendingGovernance::transition)
                            .filter(|transition| transition.scope() == scope);
                        if let Some(transition) = transition {
                            context.install_transition_certificate(transition, &certificate)?;
                            return Ok(true);
                        }
                        if context
                            .runtime
                            .consensus()
                            .allocation_source(&scope, certificate.statement().subject_digest())
                            .is_some()
                        {
                            // An already registered allocation has its own
                            // authoritative verifier and does not need another pull.
                            return Ok(false);
                        }
                        certificate
                            .verify(&snapshot.validator_set)
                            .map_err(BftConsensusRuntimeError::Finality)?;
                        context.runtime.begin_prepared_task_fetch(
                            validator_id,
                            snapshot.validator_set.version(),
                            scope.clone(),
                            certificate.statement().subject_digest(),
                            0,
                        );
                        Ok(false)
                    });
                    match installed {
                        Ok(true) => context.runtime.finish_prepared_task_sync(&scope),
                        Ok(false) => pass.push(InboundBftMessage {
                            validator_id,
                            message: BftNetworkMessage::FinalityCertificate { scope, certificate },
                        }),
                        Err(error) => context.runtime.consensus().record_rejection(
                            Some(validator_id),
                            scope,
                            node_error_to_consensus(error),
                        ),
                    }
                }
                BftNetworkMessage::ValidatorSetTransitionSource {
                    collecting,
                    validator_set_version,
                    scope,
                    bytes,
                } => {
                    let result = install_transition_source(
                        &context,
                        validator_set_version,
                        &scope,
                        &bytes,
                        collecting,
                    );
                    if let Err(error) = result {
                        if matches!(
                            error,
                            BftConsensusRuntimeError::Persistence(
                                crate::PersistenceError::StalePreparedTasks
                                    | crate::PersistenceError::StaleState
                            )
                        ) && let Ok(source) = ValidatorSetTransitionSource::decode_bytes(&bytes)
                            && source.task_handoff_digest().is_some()
                            && let Ok(snapshot) = context.current()
                            && let Ok(transition) =
                                source.verify(&snapshot.validator_set, &snapshot.validator_registry)
                            && transition.scope() == scope
                            && transition.currency_frontier()
                                == snapshot.state.next_currency_address()
                        {
                            context.runtime.begin_prepared_task_fetch(
                                validator_id,
                                validator_set_version,
                                scope.clone(),
                                transition.digest(),
                                0,
                            );
                        }
                        context.runtime.consensus().record_rejection(
                            Some(validator_id),
                            scope,
                            error,
                        );
                    }
                }
                BftNetworkMessage::PublicCheckpointSource {
                    validator_set_version,
                    scope,
                    bytes,
                } => {
                    if let Err(error) = install_public_checkpoint_source(
                        &context,
                        validator_set_version,
                        &scope,
                        &bytes,
                    ) {
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
                    let result = result.and_then(|certified| {
                        if let Some(certified) = certified {
                            match self.publish_state_recovery_provider(&certified) {
                                Ok(()) => {
                                    self.recover_validator_safety_if_ready(&context.runtime)
                                        .map_err(node_error_to_consensus)?;
                                }
                                Err(NodeRuntimeError::Persistence(
                                    crate::PersistenceError::RecoveryCheckpointDoesNotMatchState,
                                )) => {}
                                Err(error) => return Err(node_error_to_consensus(error)),
                            }
                        }
                        Ok(())
                    });
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

fn install_transition_source(
    context: &GovernanceContext,
    validator_set_version: u64,
    scope: &ConsensusScope,
    bytes: &[u8],
    collecting: bool,
) -> Result<(), BftConsensusRuntimeError> {
    let persisted = context
        .store
        .load_shared()
        .map_err(BftConsensusRuntimeError::Persistence)?
        .ok_or(BftConsensusRuntimeError::InvalidGovernanceSource)?;
    if persisted.validator_set.version() != validator_set_version {
        return Err(BftConsensusRuntimeError::InvalidGovernanceSource);
    }
    let source = ValidatorSetTransitionSource::decode_bytes(bytes)
        .map_err(BftConsensusRuntimeError::GovernanceSourceCodec)?;
    let transition = source
        .verify(&persisted.validator_set, &persisted.validator_registry)
        .map_err(|_| BftConsensusRuntimeError::InvalidGovernanceSource)?;
    if scope != &transition.scope()
        || transition.currency_frontier() != persisted.state.next_currency_address()
    {
        return Err(BftConsensusRuntimeError::InvalidGovernanceSource);
    }
    validate_bootstrap_quorum(&persisted.validator_set, transition.next_validator_set())?;
    if context
        .runtime
        .has_collected_transition(scope, transition.digest())
    {
        return Ok(());
    }
    if collecting {
        let local = context
            .store
            .prepare_validator_set_transition(transition.clone().with_handoff_digest(None))
            .map_err(BftConsensusRuntimeError::Persistence)?;
        let local = context
            .collect_transition(&local)
            .map_err(node_error_to_consensus)?;
        if source.task_handoff_digest().is_some() && local.digest() != transition.digest() {
            return Err(BftConsensusRuntimeError::Persistence(
                PersistenceError::StalePreparedTasks,
            ));
        }
        context
            .runtime
            .remember_collected_transition(scope, transition.digest());
        return Ok(());
    }
    let transition = context
        .store
        .prepare_validator_set_transition(transition)
        .map_err(BftConsensusRuntimeError::Persistence)?;
    context
        .begin_transition(&transition)
        .map_err(node_error_to_consensus)?;
    context
        .runtime
        .remember_collected_transition(scope, transition.digest());
    context
        .runtime
        .finish_prepared_task_fetch(scope, transition.digest());
    Ok(())
}

fn install_recovery_source(
    context: &GovernanceContext,
    validator_set_version: u64,
    scope: &ConsensusScope,
    bytes: &[u8],
) -> Result<Option<crate::CertifiedStateRecoveryCheckpoint>, BftConsensusRuntimeError> {
    let proof = crate::StateRecoveryCheckpointProof::decode_bytes(bytes)
        .map_err(|_| BftConsensusRuntimeError::InvalidGovernanceSource)?;
    let checkpoint = proof.checkpoint().clone();
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
    if !proof.votes().is_empty() {
        let certified = proof
            .verify_checkpoint(&persisted.validator_set)
            .map_err(BftConsensusRuntimeError::Finality)?;
        match context
            .store
            .install_recovery_checkpoint_evidence(&certified)
        {
            Ok(_) => {}
            Err(crate::PersistenceError::StaleRecoveryCheckpointSerial { .. }) => return Ok(None),
            Err(error) => return Err(BftConsensusRuntimeError::Persistence(error)),
        }
        context
            .runtime
            .consensus()
            .recovery_checkpoint_installed(&certified);
        let current = context
            .store
            .load_shared()
            .map_err(BftConsensusRuntimeError::Persistence)?
            .ok_or(BftConsensusRuntimeError::InvalidGovernanceSource)?;
        for pending in current
            .pending_governance
            .values()
            .filter(|pending| matches!(pending, crate::persistence::PendingGovernance::Recovery(_)))
        {
            let Some(target) = pending.target() else {
                continue;
            };
            start_validator_consensus_target_for(&context.store, &context.runtime, target)
                .map_err(node_error_to_consensus)?;
        }
        return Ok(Some(certified));
    }
    start_validator_consensus_target_for(
        &context.store,
        &context.runtime,
        ValidatorConsensusTarget::StateRecoveryCheckpoint(checkpoint),
    )
    .map_err(node_error_to_consensus)?;
    Ok(None)
}

fn validate_bootstrap_quorum(
    current: &ValidatorSet,
    next: &ValidatorSet,
) -> Result<(), BftConsensusRuntimeError> {
    let retained = next
        .credentials()
        .filter(|credential| current.contains(credential.id()))
        .count();
    if retained < next.quorum_threshold() || next.len() > crate::MAX_DEPLOYED_VALIDATORS {
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

fn install_public_checkpoint_source(
    context: &GovernanceContext,
    version: u64,
    scope: &ConsensusScope,
    bytes: &[u8],
) -> Result<(), BftConsensusRuntimeError> {
    let proof = crate::PublicCurrencyCheckpointProof::decode_bytes(bytes)
        .map_err(|_| BftConsensusRuntimeError::InvalidGovernanceSource)?;
    let checkpoint = proof.checkpoint().clone();
    let expected = ConsensusScope::PublicCheckpoint {
        validator_set_version: version,
        epoch: checkpoint.epoch(),
    };
    let current = context.current().map_err(node_error_to_consensus)?;
    if scope != &expected
        || proof.validator_set_version() != version
        || current.validator_set.version() != version
    {
        return Err(BftConsensusRuntimeError::InvalidGovernanceSource);
    }
    if !proof.votes().is_empty() {
        let certified = proof
            .verify_checkpoint(&current.validator_set)
            .map_err(|_| BftConsensusRuntimeError::InvalidGovernanceSource)?;
        context
            .store
            .install_public_checkpoint_evidence(&certified)
            .map_err(BftConsensusRuntimeError::Persistence)?;
        context
            .runtime
            .consensus()
            .public_checkpoint_installed(&certified);
        return Ok(());
    }
    let candidate = context
        .runtime
        .consensus()
        .public_checkpoint_candidate(&context.store)
        .map_err(node_error_to_consensus)?;
    if checkpoint != candidate {
        return Err(BftConsensusRuntimeError::InvalidGovernanceSource);
    }
    if current
        .public_checkpoint_proof
        .as_ref()
        .is_some_and(|proof| proof.checkpoint() == &checkpoint)
    {
        return Ok(());
    }
    start_validator_consensus_target_for(
        &context.store,
        &context.runtime,
        ValidatorConsensusTarget::PublicCheckpoint(checkpoint),
    )
    .map_err(node_error_to_consensus)
}

#[cfg(test)]
mod tests;
