//! Retry the exact admitted source after a relevant certified resource release.
use super::*;

impl NodeRuntime {
    pub(super) fn propose_abort_for_unusable_source(
        &self,
        version: u64,
        digest: [u8; 32],
        task: &crate::VerifiedLegalTask,
        selections: &[Vec<crate::CurrencyAddress>],
        snapshot: &crate::PersistedNodeState,
    ) -> Result<bool, BftConsensusRuntimeError> {
        let Some(plan) = snapshot.prepared_tasks.get(&task.task_id()) else {
            return Ok(false);
        };
        let validators = if snapshot.validator_set.version() == version {
            &snapshot.validator_set
        } else {
            snapshot
                .retained_validator_sets
                .get(&version)
                .ok_or(BftConsensusRuntimeError::InvalidPreparedTaskSource)?
        };
        let runtime = self
            .validator_bft
            .as_ref()
            .ok_or(BftConsensusRuntimeError::InvalidPreparedTaskSource)?;
        let scope = ConsensusScope::PreparedTask(task.task_id());
        let mut digests = plan
            .unowned_candidate_digests()
            .map_err(BftConsensusRuntimeError::Preparation)?;
        digests.extend(
            plan.owned_candidate_digests()
                .map_err(BftConsensusRuntimeError::Preparation)?,
        );
        if runtime.consensus().has_pending_prepared_commit_proof(
            &scope,
            &digests,
            crate::task_abort::statement(&task.task_id(), task.request_digest(), validators)
                .subject_digest(),
            validators,
        ) {
            return Ok(false);
        }
        let store = self
            .full_store()
            .map_err(|_| BftConsensusRuntimeError::InvalidPreparedTaskSource)?;
        let mut prepared =
            PreparedTaskBook::from_tasks(store.clone(), snapshot.prepared_tasks.clone())
                .map_err(BftConsensusRuntimeError::Preparation)?;
        if !prepared
            .mark_unusable_source_for_abort(&snapshot.state, task, validators, digest, selections)
            .map_err(BftConsensusRuntimeError::Preparation)?
        {
            return Ok(false);
        }
        self.start_prepared_task_consensus(task.task_id())
            .map_err(|error| match error {
                NodeRuntimeError::BftConsensus(error) => error,
                NodeRuntimeError::Persistence(error) => {
                    BftConsensusRuntimeError::Persistence(error)
                }
                _ => BftConsensusRuntimeError::InvalidPreparedTaskSource,
            })?;
        Ok(true)
    }

    pub(super) fn fetch_certified_prepared_source(
        &self,
        sender: ValidatorId,
        message: &BftNetworkMessage,
    ) -> Result<(), BftConsensusRuntimeError> {
        let ConsensusScope::PreparedTask(task_id) = message.scope() else {
            return Ok(());
        };
        let digest = match message {
            BftNetworkMessage::QuorumCertificate(certificate)
                if certificate.statement().phase() == crate::BftPhase::Precommit =>
            {
                certificate.statement().value().digest()
            }
            BftNetworkMessage::FinalityCertificate { certificate, .. } => {
                Some(certificate.statement().subject_digest())
            }
            _ => None,
        };
        let Some(digest) = digest else {
            return Ok(());
        };
        let store = self
            .full_store()
            .map_err(|_| BftConsensusRuntimeError::InvalidPreparedTaskSource)?;
        let snapshot = store
            .load_shared()
            .map_err(BftConsensusRuntimeError::Persistence)?
            .ok_or(BftConsensusRuntimeError::InvalidPreparedTaskSource)?;
        if snapshot.state.task_succeeded(task_id.clone()) == Some(true)
            || snapshot.state.task_cancelled(task_id.clone())
            || self
                .local_prepared_subject_matches(
                    message.scope(),
                    message.validator_set_version(),
                    digest,
                )
                .map_err(|_| BftConsensusRuntimeError::InvalidPreparedTaskSource)?
        {
            return Ok(());
        }
        let validators = self
            .validator_set_by_version(message.validator_set_version())
            .map_err(|_| BftConsensusRuntimeError::InvalidPreparedTaskSource)?
            .ok_or(BftConsensusRuntimeError::InvalidPreparedTaskSource)?;
        match message {
            BftNetworkMessage::QuorumCertificate(certificate) => {
                certificate
                    .verify(&validators)
                    .map_err(crate::BftDriverError::from)?;
            }
            BftNetworkMessage::FinalityCertificate { certificate, .. } => {
                certificate
                    .verify(&validators)
                    .map_err(BftConsensusRuntimeError::Finality)?;
            }
            _ => unreachable!(),
        }
        self.validator_bft
            .as_ref()
            .ok_or(BftConsensusRuntimeError::InvalidPreparedTaskSource)?
            .begin_prepared_task_fetch(
                sender,
                message.validator_set_version(),
                message.scope().clone(),
                digest,
                message.consensus_round().unwrap_or(0),
            );
        Ok(())
    }

    pub(super) fn promote_certified_contender(
        &self,
        scope: &ConsensusScope,
        certificate: &crate::FinalityCertificate,
    ) -> Result<(), BftConsensusRuntimeError> {
        let ConsensusScope::PreparedTask(task_id) = scope else {
            return Ok(());
        };
        let store = self
            .full_store()
            .map_err(|_| BftConsensusRuntimeError::InvalidPreparedTaskSource)?;
        let snapshot = store
            .load_shared()
            .map_err(BftConsensusRuntimeError::Persistence)?
            .ok_or(BftConsensusRuntimeError::InvalidPreparedTaskSource)?;
        let Some(plan) = snapshot.prepared_tasks.get(task_id) else {
            return Ok(());
        };
        let Some(candidate) = plan
            .candidate(certificate.statement().subject_digest())
            .map_err(BftConsensusRuntimeError::Preparation)?
        else {
            return Ok(());
        };
        if candidate.commit_authorized && !plan.conflict_abort {
            return Ok(());
        }
        let runtime = self
            .validator_bft
            .as_ref()
            .ok_or(BftConsensusRuntimeError::InvalidPreparedTaskSource)?;
        let losers = store
            .reconcile_prepared_commit_finality(runtime.validator_id(), task_id, certificate)
            .map_err(BftConsensusRuntimeError::Persistence)?;
        runtime.consensus().suspend_unclaimed_task(scope);
        store
            .finalize_prepared_task(
                task_id,
                certificate.statement().subject_digest(),
                certificate,
            )
            .map_err(BftConsensusRuntimeError::Persistence)?;
        for loser in losers {
            self.start_prepared_task_consensus(loser)
                .map_err(|_| BftConsensusRuntimeError::InvalidPreparedTaskSource)?;
        }
        self.recover_certified_component(task_id)
            .map_err(|error| match error {
                NodeRuntimeError::Preparation(error) => {
                    BftConsensusRuntimeError::Preparation(error)
                }
                NodeRuntimeError::Persistence(error) => {
                    BftConsensusRuntimeError::Persistence(error)
                }
                NodeRuntimeError::BftConsensus(error) => error,
                _ => BftConsensusRuntimeError::InvalidPreparedTaskSource,
            })
    }
    pub(super) fn admit_prepared_contention(
        &self,
        prepared: &mut PreparedTaskBook,
        state: &mut crate::SecondState,
        task: &crate::VerifiedLegalTask,
        validators: &ValidatorSet,
        contention: crate::prepared::VerifiedContention,
    ) -> Result<(), BftConsensusRuntimeError> {
        let scope = ConsensusScope::PreparedTask(task.task_id());
        let digest = contention
            .prepared
            .plan_digest()
            .map_err(BftConsensusRuntimeError::Preparation)?;
        let runtime = self
            .validator_bft
            .as_ref()
            .ok_or(BftConsensusRuntimeError::InvalidPreparedTaskSource)?;
        let qc = runtime
            .consensus()
            .pending_prepared_commit_qc(&scope, digest, validators);
        let mut losers = prepared
            .admit_contention(state, task, validators, contention)
            .map_err(BftConsensusRuntimeError::Preparation)?;
        // A certificate may have arrived before its candidate's source. Once
        // admission validates the exact body, persist that decision immediately;
        // waiting for another network relay can strand a retained-resource ring.
        if let Some(certificate) = runtime
            .consensus()
            .pending_finality_certificate(&scope, digest)
        {
            return self.promote_certified_contender(&scope, &certificate);
        }
        if let Some(qc) = qc {
            losers = self
                .full_store()
                .map_err(|_| BftConsensusRuntimeError::InvalidPreparedTaskSource)?
                .reconcile_prepared_commit_qc(runtime.validator_id(), &qc)
                .map_err(BftConsensusRuntimeError::Persistence)?;
            runtime.consensus().suspend_unclaimed_task(&scope);
        }
        for loser in losers {
            self.start_prepared_task_consensus(loser)
                .map_err(|error| match error {
                    NodeRuntimeError::Persistence(error) => {
                        BftConsensusRuntimeError::Persistence(error)
                    }
                    NodeRuntimeError::BftConsensus(error) => error,
                    _ => BftConsensusRuntimeError::InvalidPreparedTaskSource,
                })?;
        }
        Ok(())
    }
    pub(super) fn observe_prepared_commit_qc(
        &self,
        sender: ValidatorId,
        certificate: &crate::BftQuorumCertificate,
    ) -> Result<(), BftConsensusRuntimeError> {
        let statement = certificate.statement();
        if !matches!(statement.scope(), ConsensusScope::PreparedTask(_))
            || statement.phase() != crate::BftPhase::Prevote
            || !matches!(statement.value(), crate::BftValue::Digest(_))
        {
            return Ok(());
        }
        let runtime = self
            .validator_bft
            .as_ref()
            .ok_or(BftConsensusRuntimeError::InvalidPreparedTaskSource)?;
        let store = self
            .full_store()
            .map_err(|_| BftConsensusRuntimeError::InvalidPreparedTaskSource)?;
        if store
            .load_shared()
            .map_err(BftConsensusRuntimeError::Persistence)?
            .is_some_and(|snapshot| match statement.scope() {
                ConsensusScope::PreparedTask(task_id) => {
                    snapshot.state.task_succeeded(task_id.clone()) == Some(true)
                        || snapshot.state.task_cancelled(task_id.clone())
                        || snapshot.prepared_tasks.get(task_id).is_some_and(|plan| {
                            plan.candidate(statement.value().digest().unwrap())
                                .ok()
                                .flatten()
                                .is_some_and(|candidate| {
                                    candidate.commit_authorized && !plan.conflict_abort
                                })
                        })
                }
                _ => false,
            })
        {
            return Ok(());
        }
        let validators = self
            .validator_set_by_version(statement.validator_set_version())
            .map_err(|_| BftConsensusRuntimeError::InvalidPreparedTaskSource)?
            .ok_or(BftConsensusRuntimeError::InvalidPreparedTaskSource)?;
        certificate
            .verify(&validators)
            .map_err(crate::BftDriverError::from)?;
        let losers = store
            .reconcile_prepared_commit_qc(runtime.validator_id(), certificate)
            .map_err(BftConsensusRuntimeError::Persistence)?;
        if store
            .load_shared()
            .map_err(BftConsensusRuntimeError::Persistence)?
            .is_some_and(|snapshot| match statement.scope() {
                ConsensusScope::PreparedTask(task_id) => {
                    snapshot.prepared_tasks.get(task_id).is_some_and(|plan| {
                        !plan.conflict_abort
                            && plan
                                .candidate(statement.value().digest().unwrap())
                                .ok()
                                .flatten()
                                .is_some_and(|candidate| !candidate.commit_authorized)
                    })
                }
                _ => false,
            })
        {
            runtime
                .consensus()
                .suspend_unclaimed_task(statement.scope());
        }
        for loser in losers {
            self.start_prepared_task_consensus(loser)
                .map_err(|_| BftConsensusRuntimeError::InvalidPreparedTaskSource)?;
        }
        if !self
            .local_prepared_subject_matches(
                statement.scope(),
                statement.validator_set_version(),
                statement.value().digest().unwrap(),
            )
            .map_err(|_| BftConsensusRuntimeError::InvalidPreparedTaskSource)?
        {
            runtime.begin_prepared_task_fetch(
                sender,
                statement.validator_set_version(),
                statement.scope().clone(),
                statement.value().digest().unwrap(),
                statement.round(),
            );
        }
        Ok(())
    }
    pub(crate) fn retry_contender(&self, task_id: &crate::TaskId) -> Result<(), NodeRuntimeError> {
        let snapshot = self
            .full_store()?
            .load_shared()?
            .ok_or(NodeRuntimeError::SnapshotMissing)?;
        let Some(plan) = snapshot.prepared_tasks.get(task_id) else {
            return Ok(());
        };
        if plan.phase == crate::prepared_plan::PreparedTaskPhase::Finalized {
            return self.recover_certified_component(task_id);
        }
        if plan.commit_authorized {
            if !plan.conflict_abort {
                // A certified retirement can make the known source impossible
                // to establish on witnesses even while our old prerequisite
                // remains usable. Validate locally before choosing Abort.
                if plan.has_unavailable_transfer_address(&snapshot.state) {
                    let source =
                        crate::prepared::source::PreparedTaskSource::decode(&plan.encode_source()?)
                            .ok_or(PreparationError::InvalidPreparedPlan)?;
                    let verified = source
                        .task
                        .verify(
                            self.validator_bft
                                .as_ref()
                                .ok_or(NodeRuntimeError::ValidatorBftNotConfigured)?
                                .authorizers(),
                        )
                        .map_err(BftConsensusRuntimeError::Authorization)?;
                    if self.propose_abort_for_unusable_source(
                        plan.validator_set_version,
                        plan.plan_digest()?,
                        &verified,
                        &source.selections,
                        &snapshot,
                    )? {
                        return Ok(());
                    }
                }
                for digest in plan.unowned_candidate_digests()? {
                    let candidate = plan
                        .candidate(digest)?
                        .ok_or(PersistenceError::InvalidSnapshot)?;
                    let source = candidate.encode_source()?;
                    if let Err(error) = self.install_fetched_prepared_task(
                        plan.validator_set_version,
                        &ConsensusScope::PreparedTask(task_id.clone()),
                        digest,
                        &source,
                    ) {
                        self.validator_bft
                            .as_ref()
                            .ok_or(NodeRuntimeError::ValidatorBftNotConfigured)?
                            .consensus()
                            .record_rejection(
                                None,
                                ConsensusScope::PreparedTask(task_id.clone()),
                                error,
                            );
                    }
                }
            }
            return Ok(());
        }
        if let Some(certificate) = self
            .validator_bft
            .as_ref()
            .ok_or(NodeRuntimeError::ValidatorBftNotConfigured)?
            .consensus()
            .pending_finality_certificate(
                &ConsensusScope::PreparedTask(task_id.clone()),
                plan.plan_digest()?,
            )
        {
            self.promote_certified_contender(
                &ConsensusScope::PreparedTask(task_id.clone()),
                &certificate,
            )?;
            return Ok(());
        }
        if plan.conflict_abort {
            return Ok(());
        }
        let source = plan.encode_source()?;
        let result = self.install_fetched_prepared_task(
            plan.validator_set_version,
            &ConsensusScope::PreparedTask(task_id.clone()),
            plan.plan_digest()?,
            &source,
        );
        match result {
            Ok(()) => Ok(()),
            Err(BftConsensusRuntimeError::Persistence(error)) => Err(error.into()),
            Err(error) => {
                self.validator_bft
                    .as_ref()
                    .ok_or(NodeRuntimeError::ValidatorBftNotConfigured)?
                    .consensus()
                    .record_rejection(None, ConsensusScope::PreparedTask(task_id.clone()), error);
                Ok(())
            }
        }
    }

    pub(super) fn recover_certified_component(
        &self,
        task_id: &crate::TaskId,
    ) -> Result<(), NodeRuntimeError> {
        let store = self.full_store()?;
        let outcome = PreparedTaskBook::commit_certified_component(store, task_id, None)?;
        if outcome.completed.is_empty() {
            return Ok(());
        }
        self.resume_completed_prepared_tasks(outcome.completed, outcome.retry)
    }

    pub(super) fn resume_completed_prepared_tasks(
        &self,
        completed: Vec<crate::TaskId>,
        retry: std::collections::BTreeSet<crate::TaskId>,
    ) -> Result<(), NodeRuntimeError> {
        let store = self.full_store()?;
        let snapshot = store
            .load_shared()?
            .ok_or(NodeRuntimeError::SnapshotMissing)?;
        let runtime = self
            .validator_bft
            .as_ref()
            .ok_or(NodeRuntimeError::ValidatorBftNotConfigured)?;
        runtime.consensus().restore_completed_tasks_for(
            &snapshot,
            runtime.bft_timeouts().precommit,
            completed.iter(),
            true,
        )?;
        for task in completed {
            runtime.finish_prepared_task_sync(&ConsensusScope::PreparedTask(task));
        }
        for task in retry {
            self.retry_contender(&task)?;
        }
        self.resume_currency_allocations()?;
        self.refresh_recovery_candidates()?;
        Ok(())
    }
}
