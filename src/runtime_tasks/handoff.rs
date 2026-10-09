//! Recover certified inherited obligations under their original authority.
use super::*;

#[cfg(test)]
mod tests;

impl NodeRuntime {
    pub(crate) fn resume_inherited_prepared_sources(&self) -> Result<(), NodeRuntimeError> {
        let Some(runtime) = &self.validator_bft else {
            return Ok(());
        };
        let snapshot = self
            .full_store()?
            .load_shared()?
            .ok_or(NodeRuntimeError::SnapshotMissing)?;
        if !snapshot.validator_safety_ready {
            return Ok(());
        }
        let Some(handoff) = &snapshot.state.protocol.task_handoff else {
            return Ok(());
        };
        for ((task_id, digest), inherited) in &handoff.plans {
            if snapshot.state.task_succeeded(task_id.clone()) == Some(true)
                || snapshot.state.task_cancelled(task_id.clone())
            {
                continue;
            }
            let Some(origin) = crate::persistence::resolve_validator_set(
                &snapshot.validator_set,
                &snapshot.retained_validator_sets,
                inherited.validator_set_version,
            ) else {
                continue;
            };
            if !origin.contains(runtime.validator_id())
                || origin.version() < snapshot.minimum_signing_validator_set_version
                || runtime.signer_for(origin).is_err()
            {
                continue;
            }
            let owned = snapshot
                .prepared_tasks
                .get(task_id)
                .map(|plan| plan.candidate(*digest))
                .transpose()?
                .flatten()
                .is_some_and(|candidate| candidate.commit_authorized);
            if !owned {
                self.install_fetched_prepared_task(
                    origin.version(),
                    &ConsensusScope::PreparedTask(task_id.clone()),
                    *digest,
                    &inherited.encode_source()?,
                )?;
            }
        }
        Ok(())
    }

    pub(super) fn install_handoff_terminal(
        &self,
        scope: &ConsensusScope,
        certificate: &crate::FinalityCertificate,
    ) -> Result<bool, BftConsensusRuntimeError> {
        let ConsensusScope::PreparedTask(task_id) = scope else {
            return Ok(false);
        };
        let store = self
            .full_store()
            .map_err(|_| BftConsensusRuntimeError::InvalidPreparedTaskSource)?;
        let mut snapshot = store
            .load_shared()
            .map_err(BftConsensusRuntimeError::Persistence)?
            .ok_or(BftConsensusRuntimeError::Persistence(
                PersistenceError::MissingSnapshot,
            ))?;
        if snapshot.state.task_succeeded(task_id.clone()) == Some(true)
            || snapshot
                .state
                .protocol
                .task_handoff
                .as_ref()
                .is_none_or(|handoff| handoff.task_context(task_id).is_none())
        {
            return Ok(false);
        }
        let inherited = snapshot
            .state
            .protocol
            .task_handoff
            .as_ref()
            .unwrap()
            .task_context(task_id)
            .unwrap();
        let origin = crate::persistence::resolve_validator_set(
            &snapshot.validator_set,
            &snapshot.retained_validator_sets,
            inherited.validator_set_version,
        )
        .ok_or(BftConsensusRuntimeError::Persistence(
            PersistenceError::InvalidSnapshot,
        ))?;
        if crate::task_abort::statement(task_id, inherited.request_digest, origin)
            == certificate.statement()
        {
            // Abort remains terminal even when the inherited body is local.
            // Its exact original quorum releases fences; it grants no rights.
            let runtime = self
                .validator_bft
                .as_ref()
                .ok_or(BftConsensusRuntimeError::InvalidPreparedTaskSource)?;
            let retry = store
                .contenders_for(task_id)
                .map_err(BftConsensusRuntimeError::Persistence)?;
            store
                .install_prepared_abort(task_id, certificate)
                .map_err(BftConsensusRuntimeError::Persistence)?;
            self.resume_completed_prepared_tasks(
                vec![task_id.clone()],
                retry.into_iter().collect(),
            )
            .map_err(handoff_terminal_error)?;
            runtime.consensus().restore_prepared_terminal(
                task_id,
                origin,
                certificate.clone(),
                runtime.bft_timeouts().precommit,
            );
            return Ok(true);
        }
        if self.validator_bft.is_some()
            && snapshot
                .state
                .protocol
                .task_handoff
                .as_ref()
                .unwrap()
                .plans
                .contains_key(&(task_id.clone(), certificate.statement().subject_digest()))
            && snapshot
                .prepared_tasks
                .get(task_id)
                .map(|plan| plan.candidate(certificate.statement().subject_digest()))
                .transpose()
                .map_err(BftConsensusRuntimeError::Preparation)?
                .flatten()
                .is_none()
        {
            // The installed, immutable handoff already carries the exact body.
            // Verify finality before admitting the exact historical witness.
            // Rebuilding against today's business can reject an execution that
            // was established before its payment address began retirement.
            certificate
                .verify(origin)
                .map_err(BftConsensusRuntimeError::Finality)?;
            crate::PreparedTaskBook::from_tasks(store.clone(), snapshot.prepared_tasks.clone())
                .and_then(|mut book| {
                    book.admit_inherited_witness(
                        &snapshot.state,
                        task_id,
                        certificate.statement().subject_digest(),
                    )
                })
                .map_err(BftConsensusRuntimeError::Preparation)?;
            snapshot = store
                .load_shared()
                .map_err(BftConsensusRuntimeError::Persistence)?
                .ok_or(BftConsensusRuntimeError::Persistence(
                    PersistenceError::MissingSnapshot,
                ))?;
        }
        if let Some(plan) = snapshot.prepared_tasks.get(task_id) {
            self.validator_bft
                .as_ref()
                .ok_or(BftConsensusRuntimeError::InvalidPreparedTaskSource)?;
            let digest = certificate.statement().subject_digest();
            let inherited = snapshot
                .state
                .protocol
                .task_handoff
                .as_ref()
                .and_then(|handoff| handoff.plans.get(&(task_id.clone(), digest)));
            if plan.validator_set_version >= snapshot.validator_set.version()
                || !snapshot
                    .retained_validator_sets
                    .contains_key(&plan.validator_set_version)
                || inherited.is_none_or(|inherited| {
                    inherited.validator_set_version != plan.validator_set_version
                        || inherited.request_digest != plan.request_digest
                        || inherited.source_task != plan.source_task
                })
                || plan
                    .candidate(digest)
                    .map_err(BftConsensusRuntimeError::Preparation)?
                    .is_none_or(|candidate| {
                        inherited.is_none_or(|original| candidate.operations != original.operations)
                    })
            {
                return Ok(false);
            }
            // Historical validation comes from the authenticated handoff; the
            // terminal quorum authorizes execution, not a new local claim.
            // Reuse finality verification and atomic component execution; no
            // historical signer or voting session is needed to apply a proof.
            store
                .finalize_prepared_task(task_id, digest, certificate)
                .map_err(BftConsensusRuntimeError::Persistence)?;
            self.recover_certified_component(task_id)
                .map_err(handoff_terminal_error)?;
            return Ok(true);
        }
        Ok(false)
    }
}

fn handoff_terminal_error(error: NodeRuntimeError) -> BftConsensusRuntimeError {
    match error {
        NodeRuntimeError::Preparation(error) => BftConsensusRuntimeError::Preparation(error),
        NodeRuntimeError::Persistence(error) => BftConsensusRuntimeError::Persistence(error),
        NodeRuntimeError::BftConsensus(error) => error,
        _ => BftConsensusRuntimeError::InvalidPreparedTaskSource,
    }
}
