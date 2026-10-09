//! Decode and admit authenticated frozen sources against one business snapshot.
use super::*;
use crate::legal_task_codec::{MAX_ENCODED_LEGAL_TASK_SIZE, decode_legal_task};
use crate::{PersistedNodeState, PreparationOutcome, VerifiedLegalTask};

impl NodeRuntime {
    pub(super) fn install_fetched_prepared_task(
        &self,
        validator_set_version: u64,
        scope: &ConsensusScope,
        expected_plan_digest: [u8; 32],
        source: &[u8],
    ) -> Result<(), BftConsensusRuntimeError> {
        if crate::runtime_governance::handoff_source::is_handoff_source(source) {
            return self.install_transition_handoff_source(
                validator_set_version,
                scope,
                expected_plan_digest,
                source,
            );
        }
        let (task, selections) = if matches!(scope, ConsensusScope::PreparedTask(_)) {
            let source = crate::prepared::source::PreparedTaskSource::decode(source)
                .ok_or(BftConsensusRuntimeError::InvalidPreparedTaskSource)?;
            (source.task, source.selections)
        } else {
            if source.len() > MAX_ENCODED_LEGAL_TASK_SIZE {
                return Err(BftConsensusRuntimeError::InvalidPreparedTaskSource);
            }
            (
                decode_legal_task(source)
                    .ok_or(BftConsensusRuntimeError::InvalidPreparedTaskSource)?,
                Vec::new(),
            )
        };
        let verified = task
            .verify(
                self.validator_bft
                    .as_ref()
                    .ok_or(BftConsensusRuntimeError::InvalidPreparedTaskSource)?
                    .authorizers(),
            )
            .map_err(BftConsensusRuntimeError::Authorization)?;
        if let ConsensusScope::CurrencyAllocation {
            validator_set_version: version,
            start,
        } = scope
        {
            if *version != validator_set_version {
                return Err(BftConsensusRuntimeError::InvalidPreparedTaskSource);
            }
            let runtime = self
                .validator_bft
                .as_ref()
                .ok_or(BftConsensusRuntimeError::InvalidPreparedTaskSource)?;
            let snapshot = self
                .full_store()
                .map_err(|_| BftConsensusRuntimeError::InvalidPreparedTaskSource)?
                .load_shared()
                .map_err(BftConsensusRuntimeError::Persistence)?
                .ok_or(BftConsensusRuntimeError::InvalidPreparedTaskSource)?;
            snapshot.state.authorize_task(&verified).map_err(|error| {
                BftConsensusRuntimeError::Preparation(PreparationError::Execution(error))
            })?;
            if verified.is_expired(runtime.now())
                && !snapshot
                    .state
                    .protocol
                    .task_bindings
                    .get(&verified.task_id())
                    .is_some_and(|binding| binding.allocation_task.as_ref() == Some(&task))
                && !runtime.consensus().has_certified_source(
                    scope,
                    expected_plan_digest,
                    &snapshot.validator_set,
                )
            {
                return Err(BftConsensusRuntimeError::Preparation(
                    PreparationError::Execution(crate::ExecutionError::TaskExpired),
                ));
            }
            let allocation =
                crate::CurrencyAllocation::new(&verified, *version, *start).map_err(|error| {
                    BftConsensusRuntimeError::Preparation(PreparationError::Execution(error))
                })?;
            if allocation.digest() != expected_plan_digest {
                return Err(BftConsensusRuntimeError::InvalidPreparedTaskSource);
            }
            let runtime = self
                .validator_bft
                .as_ref()
                .ok_or(BftConsensusRuntimeError::InvalidPreparedTaskSource)?;
            let store = self
                .full_store()
                .map_err(|_| BftConsensusRuntimeError::InvalidPreparedTaskSource)?;
            store
                .validate_currency_allocation(&allocation)
                .map_err(BftConsensusRuntimeError::Persistence)?;
            store
                .queue_currency_allocation(&verified)
                .map_err(BftConsensusRuntimeError::Persistence)?;
            return crate::runtime_bft_consensus::start_validator_consensus_target_for(
                store,
                runtime,
                crate::runtime_consensus_target::ValidatorConsensusTarget::CurrencyAllocation(
                    allocation,
                ),
            )
            .map_err(|_| BftConsensusRuntimeError::InvalidPreparedTaskSource);
        }
        let ConsensusScope::PreparedTask(task_id) = scope else {
            return Err(BftConsensusRuntimeError::InvalidPreparedTaskSource);
        };
        if verified.task_id() != *task_id {
            return Err(BftConsensusRuntimeError::InvalidPreparedTaskSource);
        }

        let store = self
            .full_store()
            .map_err(|_| BftConsensusRuntimeError::InvalidPreparedTaskSource)?;
        let persisted = store
            .load_shared()
            .map_err(BftConsensusRuntimeError::Persistence)?
            .ok_or(BftConsensusRuntimeError::InvalidPreparedTaskSource)?;
        self.install_verified_prepared_source(
            validator_set_version,
            expected_plan_digest,
            &verified,
            &selections,
            persisted,
        )
    }

    fn install_verified_prepared_source(
        &self,
        validator_set_version: u64,
        expected_plan_digest: [u8; 32],
        verified: &VerifiedLegalTask,
        selections: &[Vec<crate::CurrencyAddress>],
        mut persisted: std::sync::Arc<PersistedNodeState>,
    ) -> Result<(), BftConsensusRuntimeError> {
        const MAX_STALE_RETRIES: usize = 3;
        for attempt in 0..=MAX_STALE_RETRIES {
            let result = self
                .install_prepared_source_snapshot(
                    validator_set_version,
                    expected_plan_digest,
                    verified,
                    selections,
                    &persisted,
                )
                .or_else(|error| {
                    if matches!(
                        &error,
                        BftConsensusRuntimeError::Preparation(PreparationError::Execution(
                            crate::ExecutionError::AccountNotFound(_)
                                | crate::ExecutionError::AccountAlreadyExists(_)
                                | crate::ExecutionError::PaymentAddressAlreadyExists(_)
                                | crate::ExecutionError::PaymentAddressUnavailable(_)
                                | crate::ExecutionError::InvalidPaymentAddressTransition(_)
                                | crate::ExecutionError::InsufficientBalance { .. }
                                | crate::ExecutionError::CurrencyNotFound(_)
                                | crate::ExecutionError::CurrencyNotCirculation(_)
                                | crate::ExecutionError::CurrencyNotOwned(_)
                        ))
                    ) && self.propose_abort_for_unusable_source(
                        validator_set_version,
                        expected_plan_digest,
                        verified,
                        selections,
                        &persisted,
                    )? {
                        Ok(())
                    } else {
                        Err(error)
                    }
                });
            match result {
                Err(
                    BftConsensusRuntimeError::Persistence(
                        PersistenceError::StaleState | PersistenceError::StalePreparedTasks,
                    )
                    | BftConsensusRuntimeError::Preparation(PreparationError::Persistence(
                        PersistenceError::StaleState | PersistenceError::StalePreparedTasks,
                    )),
                ) if attempt < MAX_STALE_RETRIES => {
                    persisted = self
                        .full_store()
                        .map_err(|_| BftConsensusRuntimeError::InvalidPreparedTaskSource)?
                        .load_shared()
                        .map_err(BftConsensusRuntimeError::Persistence)?
                        .ok_or(BftConsensusRuntimeError::InvalidPreparedTaskSource)?;
                }
                result => return result,
            }
        }
        unreachable!("bounded source admission retry loop always returns")
    }

    fn install_prepared_source_snapshot(
        &self,
        validator_set_version: u64,
        expected_plan_digest: [u8; 32],
        verified: &VerifiedLegalTask,
        selections: &[Vec<crate::CurrencyAddress>],
        persisted: &PersistedNodeState,
    ) -> Result<(), BftConsensusRuntimeError> {
        let validator_set = if persisted.validator_set.version() == validator_set_version {
            &persisted.validator_set
        } else {
            persisted
                .retained_validator_sets
                .get(&validator_set_version)
                .ok_or(BftConsensusRuntimeError::InvalidPreparedTaskSource)?
        };
        let task_id = &verified.task_id();
        // A pull begun before collection may complete after its body has already
        // become a handoff witness. Reuse that validated body without turning
        // a duplicate announcement into a new resource owner. The running
        // membership callback resumes acquisition after the certified cut.
        if let Some(local) = persisted.prepared_tasks.get(task_id)
            && let Some(candidate) = local
                .candidate(expected_plan_digest)
                .map_err(BftConsensusRuntimeError::Preparation)?
            && !candidate.commit_authorized
            && persisted.pending_governance.values().any(|pending| {
                matches!(pending, crate::persistence::PendingGovernance::CollectingTransition(value)
                    if value.handoff.as_ref().is_some_and(|handoff|
                        handoff.plans.contains_key(&(task_id.clone(), expected_plan_digest))))
            })
        {
            let source = crate::prepared::source::PreparedTaskSource::decode(
                &candidate
                    .encode_source()
                    .map_err(BftConsensusRuntimeError::Preparation)?,
            )
            .ok_or(BftConsensusRuntimeError::InvalidPreparedTaskSource)?;
            return if candidate.validator_set_version == validator_set_version
                && candidate.request_digest == verified.request_digest()
                && &source.task == verified.signed_task()
                && source.selections == selections
            {
                Ok(())
            } else {
                Err(BftConsensusRuntimeError::InvalidPreparedTaskSource)
            };
        }
        if persisted.pending_governance.values().any(|pending| {
            matches!(pending, crate::persistence::PendingGovernance::CollectingTransition(value)
                if value.current_validator_set_version() == validator_set_version)
        }) && !persisted
            .prepared_tasks
            .get(task_id)
            .map(|plan| plan.candidate(expected_plan_digest))
            .transpose()
            .map_err(BftConsensusRuntimeError::Preparation)?
            .flatten()
            .is_some_and(|candidate| candidate.commit_authorized)
        {
            return Err(BftConsensusRuntimeError::Persistence(
                PersistenceError::TransitionCollectionIncomplete {
                    validator_set_version,
                    currency_frontier: persisted.state.next_currency_address(),
                },
            ));
        }
        let store = self
            .full_store()
            .map_err(|_| BftConsensusRuntimeError::InvalidPreparedTaskSource)?;
        let mut state = persisted.state.clone();
        let mut prepared =
            PreparedTaskBook::from_tasks(store.clone(), persisted.prepared_tasks.clone())
                .map_err(BftConsensusRuntimeError::Preparation)?;

        if crate::task_abort::statement(task_id, verified.request_digest(), validator_set)
            .subject_digest()
            == expected_plan_digest
        {
            let contention = prepared
                .verify_contention(
                    &state,
                    verified,
                    self.validator_bft
                        .as_ref()
                        .ok_or(BftConsensusRuntimeError::InvalidPreparedTaskSource)?
                        .now(),
                    validator_set,
                    expected_plan_digest,
                    selections,
                )
                .map_err(BftConsensusRuntimeError::Preparation)?;
            return self.admit_prepared_contention(
                &mut prepared,
                &mut state,
                verified,
                validator_set,
                contention,
            );
        }

        let runtime = self
            .validator_bft
            .as_ref()
            .ok_or(BftConsensusRuntimeError::InvalidPreparedTaskSource)?;
        let now = runtime.now();
        // A verified finality or Precommit quorum proves prior admission of this exact plan.
        // It preserves time eligibility, not business validity or resource rights.
        let now = if verified.is_expired(now)
            && runtime.consensus().has_certified_source(
                &ConsensusScope::PreparedTask(task_id.clone()),
                expected_plan_digest,
                validator_set,
            ) {
            0
        } else {
            now
        };
        let locally_owned = persisted
            .prepared_tasks
            .get(task_id)
            .map(|plan| plan.candidate(expected_plan_digest))
            .transpose()
            .map_err(BftConsensusRuntimeError::Preparation)?
            .flatten()
            .is_some_and(|candidate| candidate.commit_authorized);
        if !locally_owned
            && let Some(certificate) = runtime.consensus().pending_finality_certificate(
                &ConsensusScope::PreparedTask(task_id.clone()),
                expected_plan_digest,
            )
        {
            let contention = prepared
                .verify_certified_source(
                    &state,
                    verified,
                    now,
                    validator_set,
                    &certificate,
                    selections,
                )
                .map_err(BftConsensusRuntimeError::Preparation)?;
            return self.admit_prepared_contention(
                &mut prepared,
                &mut state,
                verified,
                validator_set,
                contention,
            );
        }
        match prepared.prepare_expected_plan(
            &mut state,
            verified,
            now,
            validator_set,
            expected_plan_digest,
            selections,
        ) {
            Ok(PreparationOutcome::Prepared) => {}
            Ok(PreparationOutcome::AlreadySucceeded) => {
                let receipt = persisted
                    .task_receipts
                    .get(task_id)
                    .ok_or(BftConsensusRuntimeError::InvalidPreparedTaskSource)?;
                let plan = receipt.plan();
                let original = crate::prepared::source::PreparedTaskSource::decode(
                    &receipt
                        .source()
                        .map_err(BftConsensusRuntimeError::Persistence)?,
                )
                .ok_or(BftConsensusRuntimeError::InvalidPreparedTaskSource)?;
                if plan.validator_set_version != validator_set_version
                    || &plan.source_task != verified.signed_task()
                    || plan
                        .plan_digest()
                        .map_err(BftConsensusRuntimeError::Preparation)?
                        != expected_plan_digest
                    || original.selections != selections
                {
                    return Err(BftConsensusRuntimeError::InvalidPreparedTaskSource);
                }
                return Ok(());
            }
            Err(PreparationError::AlreadyPrepared(_)) => {
                if persisted.prepared_tasks.get(task_id).is_some_and(|plan| {
                    plan.plan_digest().ok() != Some(expected_plan_digest)
                        || (!plan.commit_authorized
                            && validator_set.contains(runtime.validator_id()))
                }) {
                    if let Some(contention) = prepared
                        .admit_frozen_variant(
                            &mut state,
                            verified,
                            validator_set,
                            expected_plan_digest,
                            selections,
                        )
                        .map_err(BftConsensusRuntimeError::Preparation)?
                    {
                        return self.admit_prepared_contention(
                            &mut prepared,
                            &mut state,
                            verified,
                            validator_set,
                            contention,
                        );
                    }
                    // The candidate was accepted (or was already locally owned).
                    // Do not search for it in the pre-admission snapshot: a new
                    // frozen variant has only just been persisted.
                    self.start_prepared_task_consensus(task_id.clone())
                        .map_err(|error| match error {
                            NodeRuntimeError::BftConsensus(error) => error,
                            NodeRuntimeError::Persistence(error) => {
                                BftConsensusRuntimeError::Persistence(error)
                            }
                            NodeRuntimeError::Preparation(error) => {
                                BftConsensusRuntimeError::Preparation(error)
                            }
                            _ => BftConsensusRuntimeError::InvalidPreparedTaskSource,
                        })?;
                    return Ok(());
                }
                let local = persisted
                    .prepared_tasks
                    .get(task_id)
                    .ok_or(BftConsensusRuntimeError::InvalidPreparedTaskSource)?;
                if local.request_digest != verified.request_digest()
                    || local.source_task != *verified.signed_task()
                    || local.validator_set_version != validator_set_version
                {
                    return Err(BftConsensusRuntimeError::InvalidPreparedTaskSource);
                }
                let candidate = local
                    .candidate(expected_plan_digest)
                    .map_err(BftConsensusRuntimeError::Preparation)?
                    .ok_or(BftConsensusRuntimeError::InvalidPreparedTaskSource)?;
                let source = crate::prepared::source::PreparedTaskSource::decode(
                    &candidate
                        .encode_source()
                        .map_err(BftConsensusRuntimeError::Preparation)?,
                )
                .ok_or(BftConsensusRuntimeError::InvalidPreparedTaskSource)?;
                if source.selections != selections {
                    return Err(BftConsensusRuntimeError::InvalidPreparedTaskSource);
                }
                return Ok(());
            }
            Err(
                PreparationError::Claim(
                    crate::ClaimError::CurrencyContention { .. }
                    | crate::ClaimError::ExplicitCurrencyContention { .. }
                    | crate::ClaimError::ReserveContention { .. },
                )
                | PreparationError::AccountContention(_)
                | PreparationError::PaymentAddressContention(_)
                | PreparationError::CertifiedResourceFence { .. },
            ) => {
                let contention = prepared
                    .verify_contention(
                        &state,
                        verified,
                        now,
                        validator_set,
                        expected_plan_digest,
                        selections,
                    )
                    .map_err(BftConsensusRuntimeError::Preparation)?;
                return self.admit_prepared_contention(
                    &mut prepared,
                    &mut state,
                    verified,
                    validator_set,
                    contention,
                );
            }
            Err(error) => return Err(BftConsensusRuntimeError::Preparation(error)),
        }
        self.start_prepared_task_consensus(task_id.clone())
            .map_err(|error| match error {
                NodeRuntimeError::BftConsensus(error) => error,
                NodeRuntimeError::Persistence(error) => {
                    BftConsensusRuntimeError::Persistence(error)
                }
                NodeRuntimeError::Preparation(error) => {
                    BftConsensusRuntimeError::Preparation(error)
                }
                _ => BftConsensusRuntimeError::InvalidPreparedTaskSource,
            })?;
        Ok(())
    }
}

#[cfg(test)]
mod expiry_tests;
#[cfg(test)]
mod tests;
