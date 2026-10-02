use crate::legal_task_codec::{MAX_ENCODED_LEGAL_TASK_SIZE, decode_legal_task};
use crate::runtime_bft::{
    InboundBftMessage, PreparedTaskChunk, PreparedTaskChunkResult, ValidatorBftRuntimeError,
};
use crate::{
    BftConsensusRuntimeError, BftNetworkMessage, ConsensusScope, NodeRuntime, NodeRuntimeError,
    PersistenceError, PreparationError, PreparationOutcome, PreparedTaskBook, ValidatorId,
    ValidatorSet,
};

impl NodeRuntime {
    pub(crate) fn start_durable_prepared_consensus(&self) -> Result<(), NodeRuntimeError> {
        PreparedTaskBook::recover_finalized_from_store(&self.store)?;

        for task_id in self.store.load_prepared_tasks()?.into_keys() {
            self.start_prepared_task_consensus(task_id)?;
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

        for inbound_message in inbound {
            let sender = inbound_message.validator_id;
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
                        PreparedTaskChunkResult::Pending => {}
                        PreparedTaskChunkResult::Complete(source) => {
                            match self.install_fetched_prepared_task(
                                validator_set_version,
                                &scope,
                                expected_plan_digest,
                                &source,
                            ) {
                                Ok(()) => runtime.finish_prepared_task_fetch(&scope),
                                Err(error) => {
                                    runtime.consensus().record_rejection(
                                        Some(sender),
                                        scope.clone(),
                                        error,
                                    );
                                    runtime.reject_prepared_task_source(&scope, sender);
                                }
                            }
                        }
                        PreparedTaskChunkResult::Reject => {
                            runtime.consensus().record_rejection(
                                Some(sender),
                                scope.clone(),
                                BftConsensusRuntimeError::InvalidPreparedTaskSource,
                            );
                            runtime.reject_prepared_task_source(&scope, sender);
                        }
                    }
                }
                BftNetworkMessage::PreparedTaskSourceUnavailable { scope, .. } => {
                    runtime.reject_prepared_task_source(&scope, sender);
                }
                BftNetworkMessage::PreparedTaskAvailable {
                    validator_set_version,
                    scope,
                    expected_plan_digest,
                    round,
                } => {
                    if self.is_expected_prepared_task_proposer(
                        validator_set_version,
                        round,
                        runtime.validator_id(),
                    )? && !self.local_prepared_subject_matches(
                        &scope,
                        validator_set_version,
                        expected_plan_digest,
                    )? {
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
                        && matches!(proposal.scope(), ConsensusScope::PreparedTask(_))
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
                message => consensus.push(InboundBftMessage {
                    validator_id: sender,
                    message,
                }),
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
        let ConsensusScope::PreparedTask(task_id) = scope else {
            return Ok(false);
        };
        match self.store.prepared_bft_proposal_subject(task_id.clone()) {
            Ok(subject) => Ok(subject.validator_set_version() == validator_set_version
                && subject.digest() == expected_plan_digest),
            Err(PersistenceError::StalePreparedTasks) => Ok(false),
            Err(error) => Err(error.into()),
        }
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
            .store
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

    fn install_fetched_prepared_task(
        &self,
        validator_set_version: u64,
        scope: &ConsensusScope,
        expected_plan_digest: [u8; 32],
        source: &[u8],
    ) -> Result<(), BftConsensusRuntimeError> {
        if source.len() > MAX_ENCODED_LEGAL_TASK_SIZE {
            return Err(BftConsensusRuntimeError::InvalidPreparedTaskSource);
        }
        let task =
            decode_legal_task(source).ok_or(BftConsensusRuntimeError::InvalidPreparedTaskSource)?;
        let verified = task
            .verify(
                self.validator_bft
                    .as_ref()
                    .ok_or(BftConsensusRuntimeError::InvalidPreparedTaskSource)?
                    .authorizers(),
            )
            .map_err(BftConsensusRuntimeError::Authorization)?;
        let ConsensusScope::PreparedTask(task_id) = scope else {
            return Err(BftConsensusRuntimeError::InvalidPreparedTaskSource);
        };
        if verified.task_id() != *task_id {
            return Err(BftConsensusRuntimeError::InvalidPreparedTaskSource);
        }

        let validator_set = self
            .validator_set_by_version(validator_set_version)
            .map_err(|error| match error {
                NodeRuntimeError::Persistence(error) => {
                    BftConsensusRuntimeError::Persistence(error)
                }
                _ => BftConsensusRuntimeError::InvalidPreparedTaskSource,
            })?
            .ok_or(BftConsensusRuntimeError::InvalidPreparedTaskSource)?;

        let persisted = self
            .store
            .load()
            .map_err(BftConsensusRuntimeError::Persistence)?
            .ok_or(BftConsensusRuntimeError::InvalidPreparedTaskSource)?;
        let mut state = persisted.state;
        let mut prepared = PreparedTaskBook::new(self.store.clone())
            .map_err(BftConsensusRuntimeError::Preparation)?;

        match prepared.prepare_expected_plan(
            &mut state,
            &verified,
            self.validator_bft
                .as_ref()
                .ok_or(BftConsensusRuntimeError::InvalidPreparedTaskSource)?
                .now(),
            &validator_set,
            expected_plan_digest,
        ) {
            Ok(PreparationOutcome::Prepared) => {}
            Ok(PreparationOutcome::AlreadySucceeded) => {
                return Err(BftConsensusRuntimeError::InvalidPreparedTaskSource);
            }
            Err(PreparationError::AlreadyPrepared(_)) => {
                let subject = self
                    .store
                    .prepared_bft_proposal_subject(task_id.clone())
                    .map_err(BftConsensusRuntimeError::Persistence)?;
                if subject.validator_set_version() != validator_set_version
                    || subject.digest() != expected_plan_digest
                {
                    return Err(BftConsensusRuntimeError::InvalidPreparedTaskSource);
                }
            }
            Err(error) => return Err(BftConsensusRuntimeError::Preparation(error)),
        }

        self.start_prepared_task_consensus(task_id.clone())
            .map_err(|error| match error {
                NodeRuntimeError::BftConsensus(error) => error,
                NodeRuntimeError::Persistence(error) => {
                    BftConsensusRuntimeError::Persistence(error)
                }
                NodeRuntimeError::ValidatorBft(error) => {
                    BftConsensusRuntimeError::Signing(match error {
                        ValidatorBftRuntimeError::ConsensusKeyMismatch(validator_id) => {
                            crate::ValidatorSigningError::ConsensusSigningKeyMismatch(validator_id)
                        }
                        _ => return BftConsensusRuntimeError::InvalidPreparedTaskSource,
                    })
                }
                _ => BftConsensusRuntimeError::InvalidPreparedTaskSource,
            })
    }
}
