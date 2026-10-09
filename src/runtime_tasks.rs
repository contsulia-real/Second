use crate::runtime_bft::{InboundBftMessage, PreparedTaskChunk, PreparedTaskChunkResult};
use crate::{
    BftConsensusRuntimeError, BftNetworkMessage, ConsensusScope, NodeRuntime, NodeRuntimeError,
    PersistenceError, PreparationError, PreparedTaskBook, ValidatorId, ValidatorSet,
};

mod contention;
mod handoff;
mod source_admission;

impl NodeRuntime {
    pub(crate) fn start_durable_prepared_consensus(&self) -> Result<(), NodeRuntimeError> {
        let store = self.full_store()?;
        PreparedTaskBook::recover_finalized_from_store(store)?;

        let snapshot = store
            .load_shared()?
            .ok_or(NodeRuntimeError::SnapshotMissing)?;
        let runtime = self
            .validator_bft
            .as_ref()
            .ok_or(NodeRuntimeError::ValidatorBftNotConfigured)?;
        runtime
            .consensus()
            .restore_completed_tasks(&snapshot, runtime.bft_timeouts().precommit)?;

        for task_id in store.load_prepared_tasks()?.into_keys() {
            self.start_prepared_task_consensus(task_id)?;
        }
        for (task_id, plan) in store.load_prepared_tasks()? {
            if plan.phase == crate::prepared_plan::PreparedTaskPhase::Finalized {
                continue;
            }
            if (!plan.commit_authorized
                || plan.has_unavailable_transfer_address(&snapshot.state)
                || plan
                    .variants
                    .iter()
                    .any(|variant| !variant.commit_authorized))
                && !plan.conflict_abort
            {
                self.retry_contender(&task_id)?;
            }
        }
        Ok(())
    }

    pub(crate) fn process_prepared_task_sync(
        &self,
        inbound: Vec<InboundBftMessage>,
    ) -> Result<Vec<InboundBftMessage>, NodeRuntimeError> {
        let runtime = self
            .validator_bft
            .as_ref()
            .ok_or(NodeRuntimeError::ValidatorBftNotConfigured)?;
        let mut consensus = Vec::with_capacity(inbound.len());
        let snapshot = self
            .full_store()?
            .load_shared()?
            .ok_or(NodeRuntimeError::SnapshotMissing)?;
        let version = snapshot.validator_set.version();
        let frontier = snapshot.state.next_currency_address();
        runtime.retire_superseded_allocation_sync(version, frontier);

        for inbound_message in inbound {
            if crate::runtime_bft::superseded_allocation_scope(
                inbound_message.message.scope(),
                version,
                frontier,
            ) {
                match &inbound_message.message {
                    BftNetworkMessage::PreparedTaskAvailable { .. }
                    | BftNetworkMessage::PreparedTaskSourceChunk { .. }
                    | BftNetworkMessage::PreparedTaskSourceUnavailable { .. } => continue,
                    BftNetworkMessage::PreparedTaskRequest { .. } => {}
                    _ => {
                        // Keep proof validation and relay in consensus, but a
                        // finalized frontier no longer needs a local body fetch.
                        consensus.push(inbound_message);
                        continue;
                    }
                }
            }
            let sender = inbound_message.validator_id;
            if let BftNetworkMessage::FinalityCertificate { scope, certificate } =
                &inbound_message.message
            {
                match self.install_handoff_terminal(scope, certificate) {
                    Ok(true) => {
                        runtime.finish_prepared_task_sync(scope);
                        continue;
                    }
                    Ok(false) => {}
                    Err(error) => {
                        runtime
                            .consensus()
                            .record_rejection(Some(sender), scope.clone(), error);
                        continue;
                    }
                }
            }
            if let Err(error) =
                self.fetch_certified_prepared_source(sender, &inbound_message.message)
            {
                runtime.consensus().record_rejection(
                    Some(sender),
                    inbound_message.message.scope().clone(),
                    error,
                );
                continue;
            }
            let commit_proof = match &inbound_message.message {
                BftNetworkMessage::QuorumCertificate(certificate) => Some(certificate),
                BftNetworkMessage::Proposal {
                    proposal,
                    unlock_certificate: Some(certificate),
                } if proposal.scope() == certificate.statement().scope() => Some(certificate),
                _ => None,
            };
            if let Some(certificate) = commit_proof
                && let Err(error) = self.observe_prepared_commit_qc(sender, certificate)
            {
                runtime.consensus().record_rejection(
                    Some(sender),
                    certificate.statement().scope().clone(),
                    error,
                );
                continue;
            }
            if let BftNetworkMessage::FinalityCertificate { scope, certificate } =
                &inbound_message.message
                && let Err(error) = self.promote_certified_contender(scope, certificate)
            {
                runtime
                    .consensus()
                    .record_rejection(Some(sender), scope.clone(), error);
                continue;
            }
            match inbound_message.message {
                BftNetworkMessage::PreparedTaskRequest {
                    validator_set_version,
                    scope,
                    expected_plan_digest,
                    offset,
                } => runtime.serve_prepared_task_request(
                    sender,
                    validator_set_version,
                    scope,
                    expected_plan_digest,
                    offset,
                ),
                BftNetworkMessage::PreparedTaskSourceChunk {
                    validator_set_version,
                    scope,
                    expected_plan_digest,
                    total_len,
                    offset,
                    bytes,
                } => {
                    match runtime.ingest_prepared_task_chunk(PreparedTaskChunk {
                        sender,
                        validator_set_version,
                        scope: scope.clone(),
                        expected_plan_digest,
                        total_len,
                        offset,
                        bytes,
                    }) {
                        PreparedTaskChunkResult::Pending | PreparedTaskChunkResult::Ignored => {}
                        PreparedTaskChunkResult::Complete(source) => {
                            match self.install_fetched_prepared_task(
                                validator_set_version,
                                &scope,
                                expected_plan_digest,
                                &source,
                            ) {
                                Ok(()) => {
                                    runtime.finish_prepared_task_fetch(&scope, expected_plan_digest)
                                }
                                Err(error) => {
                                    runtime.consensus().record_rejection(
                                        Some(sender),
                                        scope.clone(),
                                        error,
                                    );
                                    runtime.reject_prepared_task_source(
                                        &scope,
                                        sender,
                                        validator_set_version,
                                        expected_plan_digest,
                                    );
                                }
                            }
                        }
                        PreparedTaskChunkResult::Reject => {
                            runtime.consensus().record_rejection(
                                Some(sender),
                                scope.clone(),
                                BftConsensusRuntimeError::InvalidPreparedTaskSource,
                            );
                            runtime.reject_prepared_task_source(
                                &scope,
                                sender,
                                validator_set_version,
                                expected_plan_digest,
                            );
                        }
                    }
                }
                BftNetworkMessage::PreparedTaskSourceUnavailable {
                    scope,
                    validator_set_version,
                    expected_plan_digest,
                } => {
                    runtime.reject_prepared_task_source(
                        &scope,
                        sender,
                        validator_set_version,
                        expected_plan_digest,
                    );
                }
                BftNetworkMessage::PreparedTaskAvailable {
                    validator_set_version,
                    scope,
                    expected_plan_digest,
                    round,
                } => {
                    if (matches!(scope, ConsensusScope::CurrencyAllocation { .. })
                        || self.is_expected_prepared_task_proposer(
                            validator_set_version,
                            round,
                            runtime.validator_id(),
                        )?)
                        && !self.local_prepared_subject_matches(
                            &scope,
                            validator_set_version,
                            expected_plan_digest,
                        )?
                    {
                        runtime.begin_prepared_task_fetch(
                            sender,
                            validator_set_version,
                            scope,
                            expected_plan_digest,
                            round,
                        );
                    }
                }
                message @ BftNetworkMessage::Proposal { .. } => {
                    if let BftNetworkMessage::Proposal { proposal, .. } = &message
                        && matches!(
                            proposal.scope(),
                            ConsensusScope::PreparedTask(_)
                                | ConsensusScope::CurrencyAllocation { .. }
                        )
                    {
                        if self.local_prepared_subject_matches(
                            proposal.scope(),
                            proposal.validator_set_version(),
                            proposal.subject_digest(),
                        )? {
                            runtime.acknowledge_prepared_task_announcement(
                                sender,
                                proposal.validator_set_version(),
                                proposal.scope(),
                                proposal.subject_digest(),
                            );
                        } else {
                            runtime.begin_prepared_task_fetch(
                                sender,
                                proposal.validator_set_version(),
                                proposal.scope().clone(),
                                proposal.subject_digest(),
                                proposal.round(),
                            );
                        }
                    }
                    consensus.push(InboundBftMessage {
                        validator_id: sender,
                        message,
                    });
                }
                message => {
                    if matches!(message.scope(), ConsensusScope::CurrencyAllocation { .. }) {
                        let digest = match &message {
                            BftNetworkMessage::Vote { statement, .. } => match statement.value() {
                                crate::BftValue::Digest(digest) => Some(digest),
                                _ => None,
                            },
                            BftNetworkMessage::FinalityVote { statement, .. } => {
                                Some(statement.subject_digest())
                            }
                            BftNetworkMessage::QuorumCertificate(certificate) => {
                                match certificate.statement().value() {
                                    crate::BftValue::Digest(digest) => Some(digest),
                                    _ => None,
                                }
                            }
                            BftNetworkMessage::FinalityCertificate { certificate, .. } => {
                                Some(certificate.statement().subject_digest())
                            }
                            _ => None,
                        };
                        if let Some(digest) = digest
                            && !runtime
                                .consensus()
                                .has_allocation_candidate(message.scope(), digest)
                        {
                            runtime.begin_prepared_task_fetch(
                                sender,
                                message.validator_set_version(),
                                message.scope().clone(),
                                digest,
                                message.consensus_round().unwrap_or(0),
                            );
                        }
                    }
                    consensus.push(InboundBftMessage {
                        validator_id: sender,
                        message,
                    });
                }
            }
        }

        runtime.retry_prepared_task_fetches(false);
        Ok(consensus)
    }

    fn local_prepared_subject_matches(
        &self,
        scope: &ConsensusScope,
        validator_set_version: u64,
        expected_plan_digest: [u8; 32],
    ) -> Result<bool, NodeRuntimeError> {
        if matches!(scope, ConsensusScope::CurrencyAllocation { .. }) {
            return Ok(self.validator_bft.as_ref().is_some_and(|runtime| {
                runtime
                    .consensus()
                    .has_allocation_candidate(scope, expected_plan_digest)
            }));
        }
        let ConsensusScope::PreparedTask(task_id) = scope else {
            return Ok(false);
        };
        let snapshot = self
            .full_store()?
            .load()?
            .ok_or(NodeRuntimeError::SnapshotMissing)?;
        if let Some(binding) = snapshot.state.protocol.task_bindings.get(task_id)
            && let crate::state::TaskOutcome::Cancelled(statement) = &binding.outcome
        {
            return Ok(statement.validator_set_version() == validator_set_version
                && statement.subject_digest() == expected_plan_digest);
        }
        let Some(prepared) = snapshot.prepared_tasks.get(task_id) else {
            return Ok(false);
        };
        if prepared.validator_set_version != validator_set_version {
            return Ok(false);
        }
        let commit_digest = prepared
            .plan_digest()
            .map_err(|_| PersistenceError::InvalidSnapshot)?;
        if commit_digest == expected_plan_digest
            || prepared.candidate(expected_plan_digest)?.is_some()
        {
            return Ok(true);
        }
        let validators = if snapshot.validator_set.version() == validator_set_version {
            &snapshot.validator_set
        } else {
            snapshot
                .retained_validator_sets
                .get(&validator_set_version)
                .ok_or(PersistenceError::InvalidSnapshot)?
        };
        // Abort has no frozen business source. Recognizing its exact local domain
        // only suppresses a useless fetch; consensus still requires admission proof.
        Ok(
            crate::task_abort::statement(task_id, prepared.request_digest, validators)
                .subject_digest()
                == expected_plan_digest,
        )
    }

    fn is_expected_prepared_task_proposer(
        &self,
        validator_set_version: u64,
        round: u64,
        local_validator_id: ValidatorId,
    ) -> Result<bool, NodeRuntimeError> {
        let Some(validator_set) = self.validator_set_by_version(validator_set_version)? else {
            return Ok(false);
        };
        Ok(validator_set.proposer(round) == local_validator_id)
    }

    fn validator_set_by_version(
        &self,
        validator_set_version: u64,
    ) -> Result<Option<ValidatorSet>, NodeRuntimeError> {
        let persisted = self
            .full_store()?
            .load()?
            .ok_or(NodeRuntimeError::SnapshotMissing)?;
        if persisted.validator_set.version() == validator_set_version {
            Ok(Some(persisted.validator_set))
        } else {
            Ok(persisted
                .retained_validator_sets
                .get(&validator_set_version)
                .cloned())
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod proof_tests;

#[cfg(test)]
mod certified_admission_tests;
