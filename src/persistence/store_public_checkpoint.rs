use super::PersistedNodeState;
use super::codec::SnapshotContents;
use super::store::StateStore;
use std::collections::BTreeSet;

use crate::{
    CertifiedPublicCurrencyCheckpoint, MAX_PUBLIC_CURRENCY_DELTA_CHANGES, PersistenceError,
    PublicCurrencyCheckpointProof, PublicCurrencyDelta, PublicCurrencyDeltaChange, SecondState,
    ValidatorSet,
};

impl StateStore {
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
                state: &latest.state,
                validator_set: &latest.validator_set,
                retained_validator_sets: &latest.retained_validator_sets,
                public_checkpoint_proof,
                public_checkpoint_baseline: latest.public_checkpoint_baseline.as_ref(),
                latest_public_delta: latest.latest_public_delta.as_ref(),
                pending_public_changes: latest.pending_public_changes.as_ref(),
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
        let latest_public_delta = public_delta_for_checkpoint(&latest, &proof)?;
        let pending_public_changes = BTreeSet::new();

        self.write_next_unlocked(
            Some(latest.generation),
            SnapshotContents {
                state: &latest.state,
                validator_set: &latest.validator_set,
                retained_validator_sets: &latest.retained_validator_sets,
                public_checkpoint_proof: Some(&proof),
                public_checkpoint_baseline: Some(&proof),
                latest_public_delta: latest_public_delta.as_ref(),
                pending_public_changes: Some(&pending_public_changes),
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

        self.write_next_unlocked(
            Some(latest.generation),
            SnapshotContents {
                state: &latest.state,
                validator_set: &latest.validator_set,
                retained_validator_sets: &latest.retained_validator_sets,
                public_checkpoint_proof: latest.public_checkpoint_proof.as_ref(),
                public_checkpoint_baseline: latest.public_checkpoint_baseline.as_ref(),
                latest_public_delta: latest.latest_public_delta.as_ref(),
                pending_public_changes: latest.pending_public_changes.as_ref(),
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
    let Some(addresses) = latest.pending_public_changes.as_ref() else {
        return Ok(None);
    };
    if addresses.len() > MAX_PUBLIC_CURRENCY_DELTA_CHANGES {
        return Ok(None);
    }
    let mut changes = Vec::with_capacity(addresses.len());
    for address in addresses {
        match latest.state.public_currency_state(*address) {
            Some(state) => changes.push(PublicCurrencyDeltaChange::Upsert(state)),
            None => changes.push(PublicCurrencyDeltaChange::Remove(*address)),
        }
    }
    PublicCurrencyDelta::new(
        from_epoch,
        base.checkpoint().summary().state_digest,
        to_epoch,
        proof.checkpoint().summary().clone(),
        changes,
    )
    .map(Some)
    .map_err(|_| PersistenceError::InvalidSnapshot)
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
