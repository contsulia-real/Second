use std::collections::{BTreeMap, BTreeSet};

use super::PersistedNodeState;
use super::bft_store::retain_active_prepared_bft_states;
use super::codec::SnapshotContents;
use super::snapshot_validation::resolve_validator_set;
use super::store::StateStore;
use super::store_public_checkpoint::{checkpoint_floor_for_write, checkpoint_matches};
use super::store_recovery::recovery_checkpoint_matches;
use crate::prepared_plan::{PreparedTask, PreparedTaskPhase};
use crate::{
    FinalityCertificate, MAX_PUBLIC_CURRENCY_DELTA_CHANGES, PersistenceError, SecondState, TaskId,
    ValidatorRegistry, ValidatorSet,
};

impl StateStore {
    pub(crate) fn save_with_prepared(
        &self,
        expected_state: &SecondState,
        state: &SecondState,
        validator_set: &ValidatorSet,
        expected_prepared_tasks: &BTreeMap<TaskId, PreparedTask>,
        prepared_tasks: &BTreeMap<TaskId, PreparedTask>,
    ) -> Result<u64, PersistenceError> {
        self.save_with_prepared_and_collection(
            expected_state,
            state,
            validator_set,
            expected_prepared_tasks,
            prepared_tasks,
            None,
        )
    }

    pub(crate) fn save_with_prepared_and_collection(
        &self,
        expected_state: &SecondState,
        state: &SecondState,
        validator_set: &ValidatorSet,
        expected_prepared_tasks: &BTreeMap<TaskId, PreparedTask>,
        prepared_tasks: &BTreeMap<TaskId, PreparedTask>,
        collection: Option<&crate::ValidatorSetTransition>,
    ) -> Result<u64, PersistenceError> {
        let _guard = self.lock()?;
        let latest = self.load_unlocked()?;

        if let Some(snapshot) = latest.as_ref() {
            if state.protocol.task_handoff != snapshot.state.protocol.task_handoff {
                return Err(PersistenceError::StaleState);
            }
            if !snapshot.state.same_persisted_state(expected_state) {
                return Err(PersistenceError::StaleState);
            }
            if &snapshot.prepared_tasks != expected_prepared_tasks {
                return Err(PersistenceError::StalePreparedTasks);
            }
            if snapshot.pending_governance.values().any(|pending| {
                if let super::PendingGovernance::Transition(value) = pending {
                    value.handoff.is_some()
                        && value.current_validator_set_version() == validator_set.version()
                        && value.currency_frontier() == snapshot.state.next_currency_address()
                } else {
                    false
                }
            }) {
                // Close local resource ownership. Independently validated
                // witnesses may still consume terminal evidence, but cannot
                // grant new ownership or change this admitted root.
                for (task, plan) in prepared_tasks {
                    let old = snapshot.prepared_tasks.get(task);
                    let covered = old.is_none_or(|old| {
                        plan.request_digest == old.request_digest
                            && plan.validator_set_version == old.validator_set_version
                    }) && std::iter::once((&plan.operations, plan.commit_authorized))
                        .chain(
                            plan.variants
                                .iter()
                                .map(|variant| (&variant.operations, variant.commit_authorized)),
                        )
                        .all(|(operations, owned)| {
                            old.is_some_and(|old| {
                                (operations == &old.operations && (!owned || old.commit_authorized))
                                    || old.variants.iter().any(|variant| {
                                        operations == &variant.operations
                                            && (!owned || variant.commit_authorized)
                                    })
                            }) || !owned
                        });
                    if !covered {
                        return Err(PersistenceError::TaskAdmissionClosed {
                            validator_set_version: validator_set.version(),
                        });
                    }
                }
                for (task_id, binding) in &state.protocol.task_bindings {
                    let Some(task) = &binding.allocation_task else {
                        continue;
                    };
                    if collection.is_some() {
                        // The task book verified the exact signed remote body.
                        continue;
                    }
                    if snapshot
                        .state
                        .protocol
                        .task_bindings
                        .get(task_id)
                        .and_then(|old| old.allocation_task.as_ref())
                        == Some(task)
                        || binding.allocation_certificate.is_some()
                        || snapshot.pending_governance.values().any(|pending| {
                            pending
                                .transition()
                                .and_then(|value| value.handoff.as_ref())
                                .is_some_and(|handoff| {
                                    handoff.requests.get(task_id) == Some(task)
                                        || handoff
                                            .task_context(task_id)
                                            .is_some_and(|plan| plan.source_task == *task)
                                })
                        })
                    {
                        continue;
                    }
                    return Err(PersistenceError::TaskAdmissionClosed {
                        validator_set_version: validator_set.version(),
                    });
                }
            }
        }

        let registry = registry_for_write(latest.as_ref(), validator_set)?;
        let mut collected_pending = None;
        if let Some(transition) = collection {
            let snapshot = latest.as_ref().ok_or(PersistenceError::MissingSnapshot)?;
            if transition.current_validator_set_version() != validator_set.version()
                || transition.currency_frontier() != state.next_currency_address()
            {
                return Err(PersistenceError::StaleState);
            }
            registry
                .validate_transition(validator_set, transition.next_validator_set())
                .map_err(|_| PersistenceError::ValidatorRegistryMismatch)?;
            if transition.handoff.is_none() {
                return Err(PersistenceError::InvalidSnapshot);
            }
            // The task book validated coverage before this write. The state/task
            // CAS above preserves that validation without rehashing the baseline.
            let mut pending = snapshot.pending_governance.clone();
            let intent = transition.clone().with_handoff_digest(None).digest();
            pending.retain(|_, value| {
                !matches!(value, super::PendingGovernance::CollectingTransition(previous)
                    if previous.clone().with_handoff_digest(None).digest() == intent)
            });
            if !pending.contains_key(&transition.digest())
                && pending.len() >= super::governance::MAX_PENDING_GOVERNANCE
            {
                return Err(PersistenceError::SnapshotTooLarge);
            }
            if snapshot
                .pending_governance
                .get(&transition.digest())
                .is_some_and(|value| {
                    matches!(value, super::PendingGovernance::CollectingTransition(previous)
                        | super::PendingGovernance::Transition(previous)
                    if previous == transition)
                })
                && snapshot.prepared_tasks == *prepared_tasks
                && snapshot.state.same_persisted_state(state)
            {
                return Ok(snapshot.generation);
            }
            pending.insert(
                transition.digest(),
                super::PendingGovernance::CollectingTransition(transition.clone()),
            );
            collected_pending = Some(pending);
        }
        let checkpoint_floor_epoch = checkpoint_floor_for_write(latest.as_ref(), None)?;
        let checkpoint = latest
            .as_ref()
            .and_then(|snapshot| snapshot.public_checkpoint_proof.clone())
            .filter(|proof| checkpoint_matches(proof, state, validator_set));
        let vote_locks = latest
            .as_ref()
            .map(|snapshot| snapshot.validator_vote_locks.clone())
            .unwrap_or_default();
        let task_receipts = latest
            .as_ref()
            .map(|snapshot| snapshot.task_receipts.clone())
            .unwrap_or_default();
        let retained_validator_sets = retained_sets_for_prepared(
            latest.as_ref(),
            validator_set,
            state,
            prepared_tasks,
            &task_receipts,
        )?;
        let recovery_checkpoint_floors = latest
            .as_ref()
            .map(|snapshot| snapshot.recovery_checkpoint_floors.clone())
            .unwrap_or_default();
        let bft_local_states = latest
            .as_ref()
            .map(|snapshot| snapshot.bft_local_states.clone())
            .unwrap_or_default();
        let public_checkpoint_baseline = latest
            .as_ref()
            .and_then(|snapshot| snapshot.public_checkpoint_baseline.clone());
        let latest_public_delta = latest
            .as_ref()
            .and_then(|snapshot| snapshot.latest_public_delta.clone());
        let pending_public_changes = latest
            .as_ref()
            .and_then(|snapshot| snapshot.pending_public_changes.clone())
            .unwrap_or_default();
        let validator_transition_proofs = latest
            .as_ref()
            .map(|snapshot| snapshot.validator_transition_proofs.clone())
            .unwrap_or_default();

        self.write_next_unlocked(
            latest.as_ref().map(|snapshot| snapshot.generation),
            SnapshotContents {
                task_receipts: &task_receipts,
                pending_governance: collected_pending
                    .as_ref()
                    .or_else(|| latest.as_ref().map(|snapshot| &snapshot.pending_governance)),
                state,
                validator_set,
                retained_validator_sets: &retained_validator_sets,
                public_checkpoint_proof: checkpoint.as_ref(),
                public_checkpoint_baseline: public_checkpoint_baseline.as_ref(),
                latest_public_delta: latest_public_delta.as_ref(),
                pending_public_changes: Some(&pending_public_changes),
                validator_transition_proofs: &validator_transition_proofs,
                recovery_checkpoint_proof: latest
                    .as_ref()
                    .and_then(|snapshot| snapshot.recovery_checkpoint_proof.as_ref())
                    .filter(|proof| {
                        recovery_checkpoint_matches(
                            proof,
                            state,
                            validator_set,
                            &registry,
                            &retained_validator_sets,
                        )
                    }),
                checkpoint_floor_epoch,
                recovery_checkpoint_floors: &recovery_checkpoint_floors,
                validator_safety_ready: latest
                    .as_ref()
                    .map(|snapshot| snapshot.validator_safety_ready)
                    .unwrap_or(true),
                minimum_signing_validator_set_version: latest
                    .as_ref()
                    .map(|snapshot| snapshot.minimum_signing_validator_set_version)
                    .unwrap_or(validator_set.version()),
                pending_validator_safety_recovery: latest
                    .as_ref()
                    .and_then(|snapshot| snapshot.pending_validator_safety_recovery.as_ref()),
                validator_registry: &registry,
                prepared_tasks,
                validator_vote_locks: &vote_locks,
                bft_local_states: &bft_local_states,
            },
        )
    }

    pub(crate) fn commit_prepared_state(
        &self,
        expected_state: &SecondState,
        state: &SecondState,
        expected_prepared_tasks: &BTreeMap<TaskId, PreparedTask>,
        prepared_tasks: &BTreeMap<TaskId, PreparedTask>,
        public_changes: &BTreeSet<crate::CurrencyAddress>,
    ) -> Result<u64, PersistenceError> {
        let _guard = self.lock()?;
        let latest = self
            .load_unlocked()?
            .ok_or(PersistenceError::MissingSnapshot)?;

        if !latest.state.same_persisted_state(expected_state) {
            return Err(PersistenceError::StaleState);
        }
        if &latest.prepared_tasks != expected_prepared_tasks {
            return Err(PersistenceError::StalePreparedTasks);
        }

        let task_receipts = super::task_receipts::after_commit(&latest, state, prepared_tasks)?;
        let retained_validator_sets = retained_sets_for_prepared(
            Some(&latest),
            &latest.validator_set,
            state,
            prepared_tasks,
            &task_receipts,
        )?;
        let checkpoint = latest
            .public_checkpoint_proof
            .as_ref()
            .filter(|proof| checkpoint_matches(proof, state, &latest.validator_set));
        let mut bft_local_states = latest.bft_local_states.clone();
        retain_active_prepared_bft_states(&mut bft_local_states, prepared_tasks);
        let pending_public_changes =
            merge_pending_public_changes(latest.pending_public_changes.as_ref(), public_changes);

        self.write_next_unlocked(
            Some(latest.generation),
            SnapshotContents {
                task_receipts: &task_receipts,
                pending_governance: Some(&latest.pending_governance),
                state,
                validator_set: &latest.validator_set,
                retained_validator_sets: &retained_validator_sets,
                public_checkpoint_proof: checkpoint,
                public_checkpoint_baseline: latest.public_checkpoint_baseline.as_ref(),
                latest_public_delta: latest.latest_public_delta.as_ref(),
                pending_public_changes: pending_public_changes.as_ref(),
                validator_transition_proofs: &latest.validator_transition_proofs,
                recovery_checkpoint_proof: latest.recovery_checkpoint_proof.as_ref().filter(
                    |proof| {
                        recovery_checkpoint_matches(
                            proof,
                            state,
                            &latest.validator_set,
                            &latest.validator_registry,
                            &retained_validator_sets,
                        )
                    },
                ),
                checkpoint_floor_epoch: latest.checkpoint_floor_epoch,
                recovery_checkpoint_floors: &latest.recovery_checkpoint_floors,
                validator_safety_ready: latest.validator_safety_ready,
                minimum_signing_validator_set_version: latest.minimum_signing_validator_set_version,
                pending_validator_safety_recovery: latest
                    .pending_validator_safety_recovery
                    .as_ref(),
                validator_registry: &latest.validator_registry,
                prepared_tasks,
                validator_vote_locks: &latest.validator_vote_locks,
                bft_local_states: &bft_local_states,
            },
        )
    }

    pub(crate) fn finalize_prepared_task(
        &self,
        task_id: &TaskId,
        expected_plan_digest: [u8; 32],
        certificate: &FinalityCertificate,
    ) -> Result<u64, PersistenceError> {
        let _guard = self.lock()?;
        let mut latest = self
            .load_unlocked()?
            .ok_or(PersistenceError::MissingSnapshot)?;

        let prepared = latest
            .prepared_tasks
            .get_mut(task_id)
            .ok_or(PersistenceError::StalePreparedTasks)?;
        if !prepared
            .candidate(expected_plan_digest)
            .map_err(|_| PersistenceError::InvalidSnapshot)?
            .is_some()
        {
            return Err(PersistenceError::StalePreparedTasks);
        }
        prepared
            .select_candidate(expected_plan_digest)
            .map_err(|_| PersistenceError::StalePreparedTasks)?;
        let actual_plan_digest = prepared
            .plan_digest()
            .map_err(|_| PersistenceError::InvalidSnapshot)?;
        if actual_plan_digest != expected_plan_digest
            || certificate.statement().subject_digest() != expected_plan_digest
            || certificate.statement().validator_set_version() != prepared.validator_set_version
        {
            return Err(PersistenceError::StalePreparedTasks);
        }

        let validator_set = resolve_validator_set(
            &latest.validator_set,
            &latest.retained_validator_sets,
            prepared.validator_set_version,
        )
        .ok_or(PersistenceError::InvalidSnapshot)?;
        certificate
            .verify(validator_set)
            .map_err(|_| PersistenceError::InvalidSnapshot)?;

        if prepared.phase == PreparedTaskPhase::Finalized {
            if prepared.finality_votes.is_none() {
                return Err(PersistenceError::InvalidSnapshot);
            }
            return Ok(latest.generation);
        }

        prepared.finalize_with_votes(certificate.votes().to_vec());
        self.write_next_unlocked(
            Some(latest.generation),
            SnapshotContents {
                task_receipts: &latest.task_receipts,
                pending_governance: Some(&latest.pending_governance),
                state: &latest.state,
                validator_set: &latest.validator_set,
                retained_validator_sets: &latest.retained_validator_sets,
                public_checkpoint_proof: latest.public_checkpoint_proof.as_ref(),
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

    pub(crate) fn replace_prepared_tasks(
        &self,
        expected_prepared_tasks: &BTreeMap<TaskId, PreparedTask>,
        prepared_tasks: &BTreeMap<TaskId, PreparedTask>,
    ) -> Result<u64, PersistenceError> {
        let _guard = self.lock()?;
        let latest = self
            .load_unlocked()?
            .ok_or(PersistenceError::MissingSnapshot)?;

        if &latest.prepared_tasks != expected_prepared_tasks {
            return Err(PersistenceError::StalePreparedTasks);
        }
        let retained_validator_sets = retained_sets_for_prepared(
            Some(&latest),
            &latest.validator_set,
            &latest.state,
            prepared_tasks,
            &latest.task_receipts,
        )?;
        let mut bft_local_states = latest.bft_local_states.clone();
        retain_active_prepared_bft_states(&mut bft_local_states, prepared_tasks);

        self.write_next_unlocked(
            Some(latest.generation),
            SnapshotContents {
                task_receipts: &latest.task_receipts,
                pending_governance: Some(&latest.pending_governance),
                state: &latest.state,
                validator_set: &latest.validator_set,
                retained_validator_sets: &retained_validator_sets,
                public_checkpoint_proof: latest.public_checkpoint_proof.as_ref(),
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
                prepared_tasks,
                validator_vote_locks: &latest.validator_vote_locks,
                bft_local_states: &bft_local_states,
            },
        )
    }

    pub(crate) fn load_prepared_tasks(
        &self,
    ) -> Result<BTreeMap<TaskId, PreparedTask>, PersistenceError> {
        Ok(self
            .load()?
            .map(|snapshot| snapshot.prepared_tasks)
            .unwrap_or_default())
    }
}

pub(super) fn retained_sets_for_prepared(
    latest: Option<&PersistedNodeState>,
    active_validator_set: &ValidatorSet,
    state: &SecondState,
    prepared_tasks: &BTreeMap<TaskId, PreparedTask>,
    task_receipts: &super::TaskReceipts,
) -> Result<BTreeMap<u64, ValidatorSet>, PersistenceError> {
    let mut referenced = super::allocation_validation::referenced_versions(state, prepared_tasks);
    referenced.extend(super::task_receipts::referenced_versions(task_receipts));
    let mut retained = BTreeMap::new();

    for version in referenced {
        if version == active_validator_set.version() {
            continue;
        }

        let set = latest
            .and_then(|snapshot| {
                if snapshot.validator_set.version() == version {
                    Some(snapshot.validator_set.clone())
                } else {
                    snapshot.retained_validator_sets.get(&version).cloned()
                }
            })
            .ok_or(PersistenceError::InvalidSnapshot)?;
        retained.insert(version, set);
    }

    Ok(retained)
}

fn registry_for_write(
    latest: Option<&PersistedNodeState>,
    validator_set: &ValidatorSet,
) -> Result<ValidatorRegistry, PersistenceError> {
    match latest {
        Some(snapshot) => {
            snapshot
                .validator_registry
                .validate_current_set(validator_set)
                .map_err(|_| PersistenceError::ValidatorRegistryMismatch)?;
            Ok(snapshot.validator_registry.clone())
        }
        None => ValidatorRegistry::from_validator_set(validator_set)
            .map_err(|_| PersistenceError::ValidatorRegistryMismatch),
    }
}

fn merge_pending_public_changes(
    current: Option<&BTreeSet<crate::CurrencyAddress>>,
    changes: &BTreeSet<crate::CurrencyAddress>,
) -> Option<BTreeSet<crate::CurrencyAddress>> {
    let mut merged = current?.clone();
    for address in changes {
        merged.insert(*address);
        if merged.len() > MAX_PUBLIC_CURRENCY_DELTA_CHANGES {
            return None;
        }
    }
    Some(merged)
}
