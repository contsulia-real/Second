use super::*;

pub(super) fn collection_subject(
    store: &crate::StateStore,
    validators: &ValidatorSet,
    target: &ValidatorConsensusTarget,
    validator_id: ValidatorId,
) -> Result<Option<(BftProposalSubject, u64)>, BftConsensusRuntimeError> {
    let ValidatorConsensusTarget::ValidatorSetTransition(value) = target else {
        return Ok(None);
    };
    let snapshot = store
        .load_shared()
        .map_err(BftConsensusRuntimeError::Persistence)?
        .ok_or(BftConsensusRuntimeError::Persistence(
            PersistenceError::MissingSnapshot,
        ))?;
    Ok((&snapshot.validator_set == validators
        && matches!(snapshot.pending_governance.get(&value.digest()),
        Some(crate::persistence::PendingGovernance::CollectingTransition(saved)) if saved == value))
    .then(|| {
        let subject = BftProposalSubject::new(
            validators.version(),
            value.scope(),
            value.clone().with_handoff_digest(None).digest(),
        );
        let round = snapshot
            .bft_local_states
            .get(&(validator_id, value.scope()))
            .filter(|state| state.validator_set_version() == validators.version())
            .map_or(0, |state| state.round());
        (subject, round)
    }))
}

pub(super) fn membership_ready(
    session: &mut BftConsensusSession,
    snapshot: &crate::PersistedNodeState,
) -> Result<bool, BftConsensusRuntimeError> {
    let sealed = |target: &ValidatorConsensusTarget| match target {
        ValidatorConsensusTarget::ValidatorSetTransition(value) => {
            matches!(snapshot.pending_governance.get(&value.digest()),
                Some(crate::persistence::PendingGovernance::Transition(saved)) if saved == value)
        }
        _ => true,
    };
    if !sealed(&session.target) {
        candidates::select_membership_union(session, snapshot)?;
    }
    Ok(sealed(&session.target))
}

impl NodeRuntime {
    pub(crate) fn resume_governance(&self) -> Result<(), NodeRuntimeError> {
        let Some(runtime) = &self.validator_bft else {
            return Ok(());
        };
        let store = self.full_store()?;
        let snapshot = store
            .load_shared()?
            .ok_or(NodeRuntimeError::SnapshotMissing)?;
        if !snapshot.validator_safety_ready {
            return Ok(());
        }
        for pending in snapshot.pending_governance.values() {
            let Some(target) = pending.target() else {
                continue;
            };
            let scope = match &target {
                ValidatorConsensusTarget::ValidatorSetTransition(value) => value.scope(),
                ValidatorConsensusTarget::StateRecoveryCheckpoint(value) => {
                    ConsensusScope::StateRecoveryCheckpoint {
                        validator_set_version: value.validator_set_version(),
                        serial: value.serial(),
                    }
                }
                _ => unreachable!(),
            };
            match start_validator_consensus_target_for(store, runtime, target) {
                Ok(()) => {}
                Err(NodeRuntimeError::Persistence(
                    error @ (PersistenceError::RecoveryCheckpointDoesNotMatchState
                    | PersistenceError::StaleState),
                )) => {
                    runtime.consensus().record_rejection(
                        None,
                        scope,
                        BftConsensusRuntimeError::Persistence(error),
                    );
                }
                Err(error) => return Err(error),
            }
        }
        self.announce_pending_transition_sources()?;
        self.advance_transition_collections()?;
        self.refresh_recovery_candidates()?;
        Ok(())
    }

    pub(crate) fn refresh_recovery_candidates(&self) -> Result<(), NodeRuntimeError> {
        let Some(runtime) = &self.validator_bft else {
            return Ok(());
        };
        let store = self.full_store()?;
        let snapshot = store
            .load_shared()?
            .ok_or(NodeRuntimeError::SnapshotMissing)?;
        if !snapshot.validator_safety_ready
            || !snapshot.pending_governance.values().any(|pending| {
                matches!(pending, crate::persistence::PendingGovernance::Recovery(_))
            })
        {
            return Ok(());
        }
        let checkpoint = store.next_state_recovery_checkpoint()?;
        match start_validator_consensus_target_for(
            store,
            runtime,
            ValidatorConsensusTarget::StateRecoveryCheckpoint(checkpoint.clone()),
        ) {
            Ok(()) => {}
            Err(NodeRuntimeError::Persistence(error @ PersistenceError::SnapshotTooLarge)) => {
                runtime.consensus().record_rejection(
                    None,
                    ConsensusScope::StateRecoveryCheckpoint {
                        validator_set_version: checkpoint.validator_set_version(),
                        serial: checkpoint.serial(),
                    },
                    BftConsensusRuntimeError::Persistence(error),
                );
                return Ok(());
            }
            Err(error) => return Err(error),
        }
        runtime.broadcast(&BftNetworkMessage::StateRecoveryCheckpointSource {
            validator_set_version: checkpoint.validator_set_version(),
            scope: ConsensusScope::StateRecoveryCheckpoint {
                validator_set_version: checkpoint.validator_set_version(),
                serial: checkpoint.serial(),
            },
            bytes: checkpoint.encode_source()?,
        });
        Ok(())
    }
}

impl ValidatorConsensusRuntime {
    pub(crate) fn retire_certified_omissions(
        &self,
        snapshot: &crate::PersistedNodeState,
    ) -> Result<(), BftConsensusRuntimeError> {
        let protected = crate::persistence::handoff_evidence_digests(snapshot)
            .map_err(BftConsensusRuntimeError::Persistence)?;
        let mut coordinator = self
            .coordinator
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let retired = coordinator
            .sessions
            .iter()
            .filter_map(|(scope, session)| {
                if let ConsensusScope::CurrencyAllocation {
                    validator_set_version,
                    ..
                } = scope
                {
                    return (*validator_set_version < snapshot.validator_set.version()
                        && snapshot
                            .validator_transition_proofs
                            .contains_key(validator_set_version))
                    .then(|| scope.clone());
                }
                let ConsensusScope::PreparedTask(task_id) = scope else {
                    return None;
                };
                let handoff = snapshot.state.protocol.task_handoff.as_ref();
                let retired_variant = snapshot.prepared_tasks.get(task_id).is_some_and(|plan| {
                    session.candidates.keys().any(|digest| {
                        plan.candidate(*digest)
                            .ok()
                            .flatten()
                            .is_some_and(|candidate| {
                                !candidate.commit_authorized
                                    && handoff.is_none_or(|root| {
                                        !root.plans.contains_key(&(task_id.clone(), *digest))
                                    })
                            })
                    })
                });
                let retired_request = !snapshot.prepared_tasks.contains_key(task_id)
                    && snapshot
                        .state
                        .protocol
                        .task_bindings
                        .get(task_id)
                        .is_some_and(|binding| {
                            !binding.outcome.is_terminal() && binding.allocation_task.is_some()
                        })
                    && handoff.is_none_or(|root| root.task_context(task_id).is_none());
                (session.validator_set.version() < snapshot.validator_set.version()
                    && snapshot
                        .validator_transition_proofs
                        .contains_key(&session.validator_set.version())
                    && (retired_request || (retired_variant && !protected.contains_key(task_id))))
                .then(|| scope.clone())
            })
            .collect::<Vec<_>>();
        for scope in retired {
            // The durable cut has already retired this exact old origin. Do not
            // merge its buffered old votes into the new committee's task session.
            coordinator.sessions.remove(&scope);
            coordinator.pending_unregistered.remove(&scope);
        }
        Ok(())
    }

    pub(crate) fn collection_completed(&self, scope: &ConsensusScope, durable_round: u64) -> bool {
        self.coordinator
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .sessions
            .get(scope)
            .is_some_and(|session| {
                session
                    .collection_started_at_round
                    .is_some_and(|start| durable_round > start)
            })
    }
    pub(crate) fn recovery_checkpoint_installed(
        &self,
        certified: &CertifiedStateRecoveryCheckpoint,
    ) {
        let checkpoint = certified.checkpoint();
        let version = checkpoint.validator_set_version();
        let serial = checkpoint.serial();
        let mut coordinator = self
            .coordinator
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let scope = ConsensusScope::StateRecoveryCheckpoint {
            validator_set_version: version,
            serial,
        };
        if let Some(session) = coordinator.sessions.remove(&scope)
            && session.candidates.contains_key(&checkpoint.digest())
            && !session.certified_emitted
        {
            coordinator.record_event(BftConsensusEvent::CertifiedStateRecoveryCheckpoint(
                certified.clone(),
            ));
        }
        let obsolete = |scope: &ConsensusScope| {
            matches!(scope, ConsensusScope::StateRecoveryCheckpoint { validator_set_version, serial: pending }
            if *validator_set_version == version && *pending <= serial)
        };
        coordinator.sessions.retain(|scope, _| !obsolete(scope));
        coordinator
            .pending_unregistered
            .retain(|scope, _| !obsolete(scope));
        drop(coordinator);
        self.wake();
    }
}

pub(super) fn recovery_proof_message(
    checkpoint: &StateRecoveryCheckpoint,
    certificate: &FinalityCertificate,
) -> Result<BftNetworkMessage, BftConsensusRuntimeError> {
    let bytes =
        crate::StateRecoveryCheckpointProof::new(checkpoint.clone(), certificate.votes().to_vec())
            .encode_bytes()
            .map_err(|_| BftConsensusRuntimeError::InvalidGovernanceSource)?;
    Ok(BftNetworkMessage::StateRecoveryCheckpointSource {
        validator_set_version: checkpoint.validator_set_version(),
        scope: ConsensusScope::StateRecoveryCheckpoint {
            validator_set_version: checkpoint.validator_set_version(),
            serial: checkpoint.serial(),
        },
        bytes,
    })
}
