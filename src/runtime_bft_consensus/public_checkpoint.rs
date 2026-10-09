use super::*;

impl ValidatorConsensusRuntime {
    pub(crate) fn public_checkpoint_installed(
        &self,
        checkpoint: &CertifiedPublicCurrencyCheckpoint,
    ) {
        let version = checkpoint.certificate().statement().validator_set_version();
        let epoch = checkpoint.checkpoint().epoch();
        let scope = ConsensusScope::PublicCheckpoint {
            validator_set_version: version,
            epoch,
        };
        let mut coordinator = self
            .coordinator
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(session) = coordinator.sessions.remove(&scope)
            && session.subject.digest() == checkpoint.checkpoint().digest()
            && !session.certified_emitted
        {
            coordinator.record_event(BftConsensusEvent::CertifiedPublicCheckpoint(
                checkpoint.clone(),
            ));
        }
        coordinator.sessions.retain(|scope, _| {
            !matches!(scope, ConsensusScope::PublicCheckpoint {
            validator_set_version, epoch: pending,
        } if *validator_set_version != version || *pending <= epoch)
        });
        coordinator.pending_unregistered.retain(|scope, _| {
            !matches!(scope, ConsensusScope::PublicCheckpoint {
            validator_set_version, epoch: pending,
        } if *validator_set_version != version || *pending <= epoch)
        });
        drop(coordinator);
        self.wake();
    }

    pub(crate) fn public_checkpoint_candidate(
        &self,
        store: &crate::StateStore,
    ) -> Result<PublicCurrencyCheckpoint, NodeRuntimeError> {
        let snapshot = store
            .load_shared()?
            .ok_or(PersistenceError::MissingSnapshot)?;
        let mut checkpoint = store.next_public_currency_checkpoint()?;
        if snapshot
            .public_checkpoint_proof
            .as_ref()
            .is_some_and(|proof| proof.checkpoint().epoch() >= snapshot.checkpoint_floor_epoch)
        {
            return Ok(checkpoint);
        }
        let version = snapshot.validator_set.version();
        let mut coordinator = self
            .coordinator
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut epoch = checkpoint.epoch();
        for session in coordinator.sessions.values() {
            if let ValidatorConsensusTarget::PublicCheckpoint(pending) = &session.target
                && session.validator_set.version() == version
            {
                if pending.summary() == checkpoint.summary()
                    && pending.epoch() >= snapshot.checkpoint_floor_epoch
                {
                    return Ok(pending.clone());
                }
                epoch = epoch.max(
                    pending
                        .epoch()
                        .checked_add(1)
                        .ok_or(PersistenceError::GenerationOverflow)?,
                );
            }
        }
        coordinator
            .sessions
            .retain(|_, session| match &session.target {
                ValidatorConsensusTarget::PublicCheckpoint(pending) => {
                    session.validator_set.version() == version
                        && pending.summary() == checkpoint.summary()
                }
                _ => true,
            });
        if epoch != checkpoint.epoch() {
            checkpoint = PublicCurrencyCheckpoint::new(
                crate::CURRENT_PROTOCOL_VERSION,
                epoch,
                checkpoint.summary().clone(),
            );
        }
        Ok(checkpoint)
    }
}

impl NodeRuntime {
    pub(crate) fn resume_public_checkpoint(&self) -> Result<(), NodeRuntimeError> {
        let Some(runtime) = &self.validator_bft else {
            return Ok(());
        };
        let store = self.full_store()?;
        let snapshot = store
            .load_shared()?
            .ok_or(PersistenceError::MissingSnapshot)?;
        if snapshot
            .public_checkpoint_proof
            .as_ref()
            .is_some_and(|proof| proof.checkpoint().epoch() >= snapshot.checkpoint_floor_epoch)
            || !snapshot.validator_safety_ready
        {
            return Ok(());
        }
        if !snapshot.bft_local_states.keys().chain(snapshot.validator_vote_locks.keys()).any(|(_, scope)|
            matches!(scope, ConsensusScope::PublicCheckpoint { validator_set_version, epoch }
                if *validator_set_version == snapshot.validator_set.version() && *epoch > snapshot.checkpoint_floor_epoch)) {
            return Ok(());
        }
        let checkpoint = store.next_public_currency_checkpoint()?;
        let scope = ConsensusScope::PublicCheckpoint {
            validator_set_version: snapshot.validator_set.version(),
            epoch: checkpoint.epoch(),
        };
        if snapshot
            .bft_local_states
            .keys()
            .any(|(_, existing)| existing == &scope)
            || snapshot
                .validator_vote_locks
                .keys()
                .any(|(_, existing)| existing == &scope)
        {
            start_validator_consensus_target_for(
                store,
                runtime,
                ValidatorConsensusTarget::PublicCheckpoint(checkpoint),
            )?;
        }
        Ok(())
    }
}
