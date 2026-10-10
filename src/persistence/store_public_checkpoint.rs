use super::PersistedNodeState;
use super::codec::SnapshotContents;
use super::store::StateStore;

use crate::{
    CertifiedPublicCurrencyCheckpoint, PersistenceError, PublicCurrencyCheckpointProof,
    PublicCurrencyDelta, SecondState, ValidatorSet,
};

impl StateStore {
    pub fn next_public_currency_checkpoint(
        &self,
    ) -> Result<crate::PublicCurrencyCheckpoint, PersistenceError> {
        let snapshot = self
            .load_shared()?
            .ok_or(PersistenceError::MissingSnapshot)?;
        if let Some(proof) = &snapshot.public_checkpoint_proof
            && proof.checkpoint().epoch() >= snapshot.checkpoint_floor_epoch
        {
            return Ok(proof.checkpoint().clone());
        }
        let version = snapshot.validator_set.version();
        let summary = snapshot.state.public_currency_summary();
        let mut epoch = snapshot.checkpoint_floor_epoch;
        let mut digest = None;
        for ((_, scope), state) in &snapshot.bft_local_states {
            if let crate::ConsensusScope::PublicCheckpoint {
                validator_set_version,
                epoch: observed,
            } = scope
                && *validator_set_version == version
                && *observed >= epoch
            {
                epoch = *observed;
                digest = state.locked_digest().or_else(|| match state.prevote() {
                    Some(crate::BftValue::Digest(value)) => Some(value),
                    _ => None,
                });
            }
        }
        for ((_, scope), locked) in &snapshot.validator_vote_locks {
            if let crate::ConsensusScope::PublicCheckpoint {
                validator_set_version,
                epoch: observed,
            } = scope
                && *validator_set_version == version
                && *observed >= epoch
            {
                epoch = *observed;
                digest = Some(*locked);
            }
        }
        let candidate = crate::PublicCurrencyCheckpoint::new(
            crate::CURRENT_PROTOCOL_VERSION,
            epoch,
            summary.clone(),
        );
        if epoch > snapshot.checkpoint_floor_epoch && digest == Some(candidate.digest()) {
            return Ok(candidate);
        }
        let epoch = epoch
            .checked_add(1)
            .ok_or(PersistenceError::GenerationOverflow)?;
        Ok(crate::PublicCurrencyCheckpoint::new(
            crate::CURRENT_PROTOCOL_VERSION,
            epoch,
            summary,
        ))
    }

    pub fn attach_checkpoint_proof(
        &self,
        public_checkpoint_proof: Option<&PublicCurrencyCheckpointProof>,
    ) -> Result<u64, PersistenceError> {
        let _guard = self.lock()?;
        let latest = self
            .load_unlocked()?
            .ok_or(PersistenceError::MissingSnapshot)?;

        self.write_next_unlocked(
            Some(latest.generation),
            SnapshotContents {
                task_receipts: &latest.task_receipts,
                pending_governance: Some(&latest.pending_governance),
                state: &latest.state,
                validator_set: &latest.validator_set,
                retained_validator_sets: &latest.retained_validator_sets,
                public_checkpoint_proof,
                public_checkpoint_baseline: latest.public_checkpoint_baseline.as_ref(),
                latest_public_delta: latest.latest_public_delta.as_ref(),
                public_checkpoint_states: latest.public_checkpoint_states.as_ref(),
                validator_transition_proofs: &latest.validator_transition_proofs,
                recovery_checkpoint_proof: latest.recovery_checkpoint_proof.as_ref(),
                checkpoint_floor_epoch: latest.checkpoint_floor_epoch,
                recovery_checkpoint_floors: &latest.recovery_checkpoint_floors,
                validator_safety_ready: latest.validator_safety_ready,
                minimum_signing_validator_set_version: latest.minimum_signing_validator_set_version,
                pending_validator_safety_recovery: latest
                    .pending_validator_safety_recovery
                    .as_ref(),
                validator_registry: &latest.validator_registry,
                prepared_tasks: &latest.prepared_tasks,
                validator_vote_locks: &latest.validator_vote_locks,
                bft_local_states: &latest.bft_local_states,
            },
        )
    }

    pub fn attach_certified_checkpoint(
        &self,
        certified_checkpoint: &CertifiedPublicCurrencyCheckpoint,
    ) -> Result<u64, PersistenceError> {
        let _guard = self.lock()?;
        let latest = self
            .load_unlocked()?
            .ok_or(PersistenceError::MissingSnapshot)?;

        validate_certified_checkpoint_for_state(
            &latest.state,
            &latest.validator_set,
            certified_checkpoint,
        )?;
        let proof = certified_checkpoint.to_unverified_proof();
        let checkpoint_floor_epoch = checkpoint_floor_for_write(Some(&latest), Some(&proof))?;
        if latest
            .public_checkpoint_proof
            .as_ref()
            .is_some_and(|proof| {
                proof.checkpoint() == certified_checkpoint.checkpoint()
                    && proof.validator_set_version() == latest.validator_set.version()
            })
        {
            return Ok(latest.generation);
        }
        let latest_public_delta = public_delta_for_checkpoint(&latest, &proof)?;
        let public_checkpoint_states = latest.state.public_currency_states();

        self.write_next_unlocked(
            Some(latest.generation),
            SnapshotContents {
                task_receipts: &latest.task_receipts,
                pending_governance: Some(&latest.pending_governance),
                state: &latest.state,
                validator_set: &latest.validator_set,
                retained_validator_sets: &latest.retained_validator_sets,
                public_checkpoint_proof: Some(&proof),
                public_checkpoint_baseline: Some(&proof),
                latest_public_delta: latest_public_delta.as_ref(),
                public_checkpoint_states: Some(&public_checkpoint_states),
                validator_transition_proofs: &latest.validator_transition_proofs,
                recovery_checkpoint_proof: latest.recovery_checkpoint_proof.as_ref(),
                checkpoint_floor_epoch,
                recovery_checkpoint_floors: &latest.recovery_checkpoint_floors,
                validator_safety_ready: latest.validator_safety_ready,
                minimum_signing_validator_set_version: latest.minimum_signing_validator_set_version,
                pending_validator_safety_recovery: latest
                    .pending_validator_safety_recovery
                    .as_ref(),
                validator_registry: &latest.validator_registry,
                prepared_tasks: &latest.prepared_tasks,
                validator_vote_locks: &latest.validator_vote_locks,
                bft_local_states: &latest.bft_local_states,
            },
        )
    }

    pub(crate) fn install_public_checkpoint_evidence(
        &self,
        checkpoint: &CertifiedPublicCurrencyCheckpoint,
    ) -> Result<(), PersistenceError> {
        match self.attach_certified_checkpoint(checkpoint) {
            Ok(_) | Err(PersistenceError::StaleCheckpointEpoch { .. }) => Ok(()),
            Err(PersistenceError::CheckpointDoesNotMatchState) => {
                match self.advance_checkpoint_floor(checkpoint) {
                    Ok(_) | Err(PersistenceError::StaleCheckpointEpoch { .. }) => Ok(()),
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error),
        }
    }

    pub fn advance_checkpoint_floor(
        &self,
        checkpoint: &CertifiedPublicCurrencyCheckpoint,
    ) -> Result<u64, PersistenceError> {
        let _guard = self.lock()?;
        let latest = self
            .load_unlocked()?
            .ok_or(PersistenceError::MissingSnapshot)?;

        checkpoint
            .certificate()
            .verify(&latest.validator_set)
            .map_err(PersistenceError::CheckpointFinality)?;

        let proof = checkpoint.to_unverified_proof();
        let checkpoint_floor_epoch = checkpoint_floor_for_write(Some(&latest), Some(&proof))?;

        if checkpoint_floor_epoch == latest.checkpoint_floor_epoch {
            return Ok(latest.generation);
        }
        let attached = latest
            .public_checkpoint_proof
            .as_ref()
            .filter(|proof| proof.checkpoint().epoch() >= checkpoint_floor_epoch);

        self.write_next_unlocked(
            Some(latest.generation),
            SnapshotContents {
                task_receipts: &latest.task_receipts,
                pending_governance: Some(&latest.pending_governance),
                state: &latest.state,
                validator_set: &latest.validator_set,
                retained_validator_sets: &latest.retained_validator_sets,
                public_checkpoint_proof: attached,
                public_checkpoint_baseline: latest.public_checkpoint_baseline.as_ref(),
                latest_public_delta: latest.latest_public_delta.as_ref(),
                public_checkpoint_states: latest.public_checkpoint_states.as_ref(),
                validator_transition_proofs: &latest.validator_transition_proofs,
                recovery_checkpoint_proof: latest.recovery_checkpoint_proof.as_ref(),
                checkpoint_floor_epoch,
                recovery_checkpoint_floors: &latest.recovery_checkpoint_floors,
                validator_safety_ready: latest.validator_safety_ready,
                minimum_signing_validator_set_version: latest.minimum_signing_validator_set_version,
                pending_validator_safety_recovery: latest
                    .pending_validator_safety_recovery
                    .as_ref(),
                validator_registry: &latest.validator_registry,
                prepared_tasks: &latest.prepared_tasks,
                validator_vote_locks: &latest.validator_vote_locks,
                bft_local_states: &latest.bft_local_states,
            },
        )
    }
}

fn public_delta_for_checkpoint(
    latest: &PersistedNodeState,
    proof: &PublicCurrencyCheckpointProof,
) -> Result<Option<PublicCurrencyDelta>, PersistenceError> {
    let Some(base) = latest.public_checkpoint_baseline.as_ref() else {
        return Ok(None);
    };
    if base.validator_set_version() != latest.validator_set.version()
        || proof.validator_set_version() != latest.validator_set.version()
    {
        return Ok(None);
    }
    let from_epoch = base.checkpoint().epoch();
    let to_epoch = proof.checkpoint().epoch();
    if to_epoch <= from_epoch {
        return Ok(None);
    }
    let Some(states) = &latest.public_checkpoint_states else {
        return Ok(None);
    };
    let base_view =
        crate::PublicCurrencyView::new(base.checkpoint().summary().clone(), states.clone())
            .map_err(|_| PersistenceError::InvalidSnapshot)?;
    let target = crate::PublicCurrencyView::new(
        proof.checkpoint().summary().clone(),
        latest.state.public_currency_states(),
    )
    .map_err(|_| PersistenceError::InvalidSnapshot)?;
    match PublicCurrencyDelta::between(from_epoch, &base_view, to_epoch, &target) {
        Ok(delta) => match delta.encode_bytes() {
            Ok(_) => Ok(Some(delta)),
            Err(crate::PublicCurrencyDeltaError::TooLarge) => Ok(None),
            Err(_) => Err(PersistenceError::InvalidSnapshot),
        },
        Err(crate::PublicCurrencyDeltaError::TooManyChanges { .. }) => Ok(None),
        Err(_) => Err(PersistenceError::InvalidSnapshot),
    }
}

fn validate_certified_checkpoint_for_state(
    state: &SecondState,
    validator_set: &ValidatorSet,
    checkpoint: &CertifiedPublicCurrencyCheckpoint,
) -> Result<(), PersistenceError> {
    if checkpoint.checkpoint().summary() != &state.public_currency_summary() {
        return Err(PersistenceError::CheckpointDoesNotMatchState);
    }

    let actual = checkpoint.certificate().statement().validator_set_version();
    if actual != validator_set.version() {
        return Err(PersistenceError::CheckpointValidatorSetMismatch {
            expected: validator_set.version(),
            actual,
        });
    }

    Ok(())
}

pub(super) fn checkpoint_floor_for_write(
    latest: Option<&PersistedNodeState>,
    new_checkpoint: Option<&PublicCurrencyCheckpointProof>,
) -> Result<u64, PersistenceError> {
    let minimum = latest
        .map(|snapshot| snapshot.checkpoint_floor_epoch)
        .unwrap_or(0);
    let Some(proof) = new_checkpoint else {
        return Ok(minimum);
    };

    let actual = proof.checkpoint().epoch();
    if actual < minimum {
        return Err(PersistenceError::StaleCheckpointEpoch { minimum, actual });
    }

    Ok(actual)
}

pub(super) fn checkpoint_matches(
    proof: &PublicCurrencyCheckpointProof,
    state: &SecondState,
    validator_set: &ValidatorSet,
) -> bool {
    proof.checkpoint().summary() == &state.public_currency_summary()
        && proof.validator_set_version() == validator_set.version()
}
