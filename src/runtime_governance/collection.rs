//! Durable contribution exchange before membership voting.
use super::*;

impl GovernanceContext {
    pub(super) fn install_transition_certificate(
        &self,
        transition: &ValidatorSetTransition,
        certificate: &crate::FinalityCertificate,
    ) -> Result<(), NodeRuntimeError> {
        let snapshot = self.current()?;
        if certificate.statement() != transition.finality_statement() {
            return Err(NodeRuntimeError::BftConsensus(
                BftConsensusRuntimeError::FinalityStatementMismatch,
            ));
        }
        let certified = crate::CertifiedValidatorSetTransition::new(
            transition.clone(),
            certificate.votes().to_vec(),
            &snapshot.validator_set,
        )
        .map_err(BftConsensusRuntimeError::ValidatorTransition)?;
        // Validate/import witnesses, then apply the exact certified root rather
        // than the possibly larger local contribution union.
        if !snapshot
            .pending_governance
            .get(&transition.digest())
            .is_some_and(|pending| pending.transition() == Some(transition))
        {
            self.stage_collection(transition, false)?;
        }
        self.store.activate_validator_set_transition_for_runtime(
            &certified,
            self.runtime.validator_id(),
        )?;
        self.runtime.refresh_authority()?;
        let installed = self.current()?;
        self.runtime
            .consensus()
            .retire_certified_omissions(&installed)?;
        self.runtime.consensus().wake();
        Ok(())
    }

    pub(crate) fn begin_transition(
        &self,
        transition: &ValidatorSetTransition,
    ) -> Result<ValidatorSetTransition, NodeRuntimeError> {
        let snapshot = self.current()?;
        if matches!(snapshot.pending_governance.get(&transition.digest()),
            Some(crate::persistence::PendingGovernance::Transition(value)) if value == transition)
            || !snapshot.validator_safety_ready
        {
            // An already admitted/locked root must survive later witnesses and
            // covered business completion without being recaptured as a new root.
            crate::PreparedTaskBook::from_tasks(
                self.store.clone(),
                snapshot.prepared_tasks.clone(),
            )?
            .admit_transition_handoff(
                &mut snapshot.state.clone(),
                transition,
                self.runtime.authorizers(),
                self.runtime.now(),
            )?;
            if snapshot.validator_safety_ready {
                self.promote_transition(transition)?;
            } else {
                // A locked member observes a fully validated root and its QC;
                // it cannot earn the local Nil round by signing before recovery.
                if matches!(snapshot.pending_governance.get(&transition.digest()),
                    Some(crate::persistence::PendingGovernance::CollectingTransition(saved)) if saved == transition)
                {
                    self.store.promote_transition_collection(transition)?;
                }
                start_validator_consensus_target_for(
                    &self.store,
                    &self.runtime,
                    ValidatorConsensusTarget::ValidatorSetTransition(transition.clone()),
                )?;
            }
            return Ok(transition.clone());
        }
        let collected = self.stage_collection(transition, true)?;
        self.start_collection(&collected)?;
        Ok(collected)
    }

    pub(super) fn promote_transition(
        &self,
        transition: &ValidatorSetTransition,
    ) -> Result<(), NodeRuntimeError> {
        let changed = self.store.promote_transition_collection(transition)?;
        start_validator_consensus_target_for(
            &self.store,
            &self.runtime,
            ValidatorConsensusTarget::ValidatorSetTransition(transition.clone()),
        )?;
        if changed {
            self.announce_transition(transition, false)?;
        }
        Ok(())
    }

    pub(super) fn collect_transition(
        &self,
        transition: &ValidatorSetTransition,
    ) -> Result<ValidatorSetTransition, NodeRuntimeError> {
        let collected = self.stage_collection(transition, true)?;
        self.start_collection(&collected)?;
        Ok(collected)
    }

    fn start_collection(
        &self,
        transition: &ValidatorSetTransition,
    ) -> Result<(), NodeRuntimeError> {
        let snapshot = self.current()?;
        if !snapshot.validator_safety_ready {
            return Ok(());
        }
        self.runtime.refresh_authority()?;
        self.runtime.consensus().register(
            self.runtime.signer_for(&snapshot.validator_set)?,
            self.store.clone(),
            snapshot.validator_set.clone(),
            ValidatorConsensusTarget::ValidatorSetTransition(transition.clone()),
            self.runtime.bft_timeouts(),
        )?;
        Ok(())
    }

    fn stage_collection(
        &self,
        transition: &ValidatorSetTransition,
        announce: bool,
    ) -> Result<ValidatorSetTransition, NodeRuntimeError> {
        const MAX_STALE_RETRIES: usize = 3;
        for attempt in 0..=MAX_STALE_RETRIES {
            let before = self.current()?;
            if !before.validator_set.contains(self.runtime.validator_id()) {
                return Err(NodeRuntimeError::BftConsensus(
                    BftConsensusRuntimeError::InvalidGovernanceSource,
                ));
            }
            let mut state = before.state.clone();
            let result = crate::PreparedTaskBook::from_tasks(
                self.store.clone(),
                before.prepared_tasks.clone(),
            )?
            .collect_transition_handoff(
                &mut state,
                transition,
                self.runtime.authorizers(),
                self.runtime.now(),
            );
            let collected = match result {
                Err(crate::PreparationError::Persistence(
                    crate::PersistenceError::StaleState
                    | crate::PersistenceError::StalePreparedTasks,
                )) if attempt < MAX_STALE_RETRIES => continue,
                result => result?,
            };
            if self.current()?.generation != before.generation {
                if announce {
                    self.announce_collection(&collected)?;
                }
                self.runtime.consensus().wake();
            }
            return Ok(collected);
        }
        unreachable!("bounded collection retry loop always returns")
    }

    pub(super) fn announce_collection(
        &self,
        transition: &ValidatorSetTransition,
    ) -> Result<(), NodeRuntimeError> {
        self.announce_transition(transition, true)
    }

    fn announce_transition(
        &self,
        transition: &ValidatorSetTransition,
        collecting: bool,
    ) -> Result<(), NodeRuntimeError> {
        let message = BftNetworkMessage::ValidatorSetTransitionSource {
            collecting,
            validator_set_version: transition.current_validator_set_version(),
            scope: transition.scope(),
            bytes: ValidatorSetTransitionSource::from_transition(transition)
                .encode_bytes()
                .map_err(|error| {
                    NodeRuntimeError::BftConsensus(BftConsensusRuntimeError::GovernanceSourceCodec(
                        error,
                    ))
                })?,
        };
        let failures = self.runtime.broadcast(&message);
        if !failures.is_empty() {
            self.runtime
                .consensus()
                .record_send_failures(message.scope().clone(), failures);
        }
        Ok(())
    }
}

impl NodeRuntime {
    pub(crate) fn advance_transition_collections(&self) -> Result<(), NodeRuntimeError> {
        let Some(context) = self.governance_context() else {
            return Ok(());
        };
        let snapshot = context.current()?;
        if !snapshot.validator_safety_ready {
            return Ok(());
        }
        for pending in snapshot.pending_governance.values() {
            if let crate::persistence::PendingGovernance::CollectingTransition(transition) = pending
            {
                // Collection uses the membership scope's existing first BFT
                // round, voting only Nil. No body is sealed on its first wake;
                // an offline member cannot prevent the normal round advance.
                // A singleton already has every committee contribution. There
                // is no remote body to collect or reason for another Nil round.
                let collected = snapshot.validator_set.len() == 1
                    || snapshot
                        .bft_local_states
                        .get(&(context.runtime.validator_id(), transition.scope()))
                        .is_some_and(|state| {
                            state.validator_set_version() == snapshot.validator_set.version()
                                && context
                                    .runtime
                                    .consensus()
                                    .collection_completed(&transition.scope(), state.round())
                        });
                let result = if collected {
                    context.promote_transition(transition)
                } else {
                    context.start_collection(transition)
                };
                match result {
                    Ok(()) => {}
                    Err(NodeRuntimeError::Persistence(
                        error @ (PersistenceError::StaleState
                        | PersistenceError::StalePreparedTasks),
                    )) => {
                        context.runtime.consensus().record_rejection(
                            None,
                            transition.scope(),
                            BftConsensusRuntimeError::Persistence(error),
                        );
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        Ok(())
    }

    /// Persist contribution exchange; the running consensus loop promotes its
    /// validated root and selects it through the existing membership BFT.
    pub fn collect_validator_set_transition(
        &self,
        transition: ValidatorSetTransition,
    ) -> Result<ValidatorSetTransition, NodeRuntimeError> {
        let context = self
            .governance_context()
            .ok_or(NodeRuntimeError::ValidatorBftNotConfigured)?;
        // A supplied contribution is validated and unioned by collection. The
        // voting hydrator requires coverage already and cannot precede that union.
        let transition = if transition.handoff.is_some() {
            transition
        } else {
            context.store.prepare_validator_set_transition(transition)?
        };
        context.collect_transition(&transition)
    }

    pub(crate) fn has_pending_transition_sources(&self) -> Result<bool, NodeRuntimeError> {
        Ok(self
            .full_store()?
            .load_shared()?
            .ok_or(NodeRuntimeError::SnapshotMissing)?
            .pending_governance
            .values()
            .any(|value| value.transition().is_some()))
    }

    pub(crate) fn announce_pending_transition_sources(&self) -> Result<(), NodeRuntimeError> {
        let Some(context) = self.governance_context() else {
            return Ok(());
        };
        let snapshot = context.current()?;
        for pending in
            crate::persistence::PendingGovernance::transition_sources(&snapshot.pending_governance)
        {
            if let Some(transition) = pending.transition() {
                context.announce_transition(
                    transition,
                    matches!(
                        pending,
                        crate::persistence::PendingGovernance::CollectingTransition(_)
                    ),
                )?;
            }
        }
        Ok(())
    }
}
