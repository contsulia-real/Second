use super::*;

pub(super) fn message_digest(message: &BftNetworkMessage) -> Option<[u8; 32]> {
    match message {
        BftNetworkMessage::Proposal { proposal, .. } => Some(proposal.subject_digest()),
        BftNetworkMessage::Vote { statement, .. } => match statement.value() {
            BftValue::Digest(digest) => Some(digest),
            _ => None,
        },
        BftNetworkMessage::QuorumCertificate(certificate) => {
            match certificate.statement().value() {
                BftValue::Digest(digest) => Some(digest),
                _ => None,
            }
        }
        BftNetworkMessage::FinalityVote { statement, .. } => Some(statement.subject_digest()),
        BftNetworkMessage::FinalityCertificate { certificate, .. } => {
            Some(certificate.statement().subject_digest())
        }
        _ => None,
    }
}

pub(super) fn waiting_for_candidate(
    session: &BftConsensusSession,
    message: &BftNetworkMessage,
) -> bool {
    matches!(
        session.subject.scope(),
        ConsensusScope::CurrencyAllocation { .. }
            | ConsensusScope::StateRecoveryCheckpoint { .. }
            | ConsensusScope::PreparedTask(_)
    ) && message_digest(message).is_some_and(|digest| !session.candidates.contains_key(&digest))
}

pub(super) fn select_candidate(
    session: &mut BftConsensusSession,
    digest: [u8; 32],
) -> Result<(), BftConsensusRuntimeError> {
    if !matches!(
        session.subject.scope(),
        ConsensusScope::CurrencyAllocation { .. }
            | ConsensusScope::StateRecoveryCheckpoint { .. }
            | ConsensusScope::PreparedTask(_)
    ) || digest == session.subject.digest()
    {
        return Ok(());
    }
    let target = session
        .candidates
        .get(&digest)
        .ok_or(BftConsensusRuntimeError::InvalidPreparedTaskSource)?
        .clone();
    session.subject = match &target {
        ValidatorConsensusTarget::CurrencyAllocation(allocation) => allocation.subject(),
        ValidatorConsensusTarget::ValidatorSetTransition(transition) => BftProposalSubject::new(
            transition.current_validator_set_version(),
            transition.scope(),
            transition.digest(),
        ),
        ValidatorConsensusTarget::StateRecoveryCheckpoint(checkpoint) => BftProposalSubject::new(
            checkpoint.validator_set_version(),
            ConsensusScope::StateRecoveryCheckpoint {
                validator_set_version: checkpoint.validator_set_version(),
                serial: checkpoint.serial(),
            },
            checkpoint.digest(),
        ),
        ValidatorConsensusTarget::PreparedTask {
            task_id,
            plan_digest,
        } => BftProposalSubject::new(
            session.validator_set.version(),
            ConsensusScope::PreparedTask(task_id.clone()),
            *plan_digest,
        ),
        _ => unreachable!(),
    };
    session.target = target;
    session.finality_votes.clear();
    Ok(())
}

// Called only when the existing highest-QC/locked-digest selection found none.
pub(super) fn select_membership_union(
    session: &mut BftConsensusSession,
    snapshot: &crate::PersistedNodeState,
) -> Result<(), BftConsensusRuntimeError> {
    let ValidatorConsensusTarget::ValidatorSetTransition(transition) = &session.target else {
        return Ok(());
    };
    let intent = transition.clone().with_handoff_digest(None).digest();
    let digest = session
        .candidates
        .values()
        .filter_map(|candidate| {
            let ValidatorConsensusTarget::ValidatorSetTransition(value) = candidate else {
                return None;
            };
            (value.clone().with_handoff_digest(None).digest() == intent
                && matches!(snapshot.pending_governance.get(&value.digest()),
                    Some(crate::persistence::PendingGovernance::Transition(saved)) if saved == value)).then(|| {
                let duties = value
                    .handoff
                    .as_ref()
                    .map_or(0, |handoff| handoff.plans.len() + handoff.requests.len());
                (duties, value.digest())
            })
        })
        .max()
        .map(|(_, digest)| digest);
    if let Some(digest) = digest {
        select_candidate(session, digest)?;
    }
    Ok(())
}

pub(super) fn remember_prevote_qc(
    session: &mut BftConsensusSession,
    certificate: &BftQuorumCertificate,
) {
    if certificate.statement().phase() == BftPhase::Prevote
        && matches!(certificate.statement().value(), BftValue::Digest(_))
        && session
            .valid_prevote_qc
            .as_ref()
            .is_none_or(|previous| previous.statement().round() < certificate.statement().round())
    {
        session.valid_prevote_qc = Some(certificate.clone());
    }
}

// A raw Abort proposal or individual vote is insufficient admission evidence.
// A verified same-committee QC/final certificate establishes a validated quorum decision.
pub(super) fn admit_certified_abort(
    session: &mut BftConsensusSession,
    message: &BftNetworkMessage,
) -> Result<(), BftConsensusRuntimeError> {
    let ConsensusScope::PreparedTask(task_id) = session.subject.scope() else {
        return Ok(());
    };
    let Some(digest) = message_digest(message) else {
        return Ok(());
    };
    if session.candidates.contains_key(&digest) {
        return Ok(());
    }
    let abort = session
        .store
        .prepared_abort_statement(task_id)
        .map_err(BftConsensusRuntimeError::Persistence)?;
    if abort.subject_digest() != digest || message.scope() != session.subject.scope() {
        return Ok(());
    }
    match message {
        BftNetworkMessage::QuorumCertificate(qc) => qc
            .verify(&session.validator_set)
            .map_err(BftDriverError::from)?,
        BftNetworkMessage::FinalityCertificate { certificate, .. } => certificate
            .verify(&session.validator_set)
            .map_err(BftConsensusRuntimeError::Finality)?,
        BftNetworkMessage::Proposal {
            unlock_certificate: Some(qc),
            ..
        } if qc.statement().scope() == session.subject.scope()
            && qc.statement().phase() == BftPhase::Prevote
            && qc.statement().value() == BftValue::Digest(digest) =>
        {
            qc.verify(&session.validator_set)
                .map_err(BftDriverError::from)?
        }
        _ => return Ok(()),
    }
    let subject = BftProposalSubject::new(
        session.validator_set.version(),
        session.subject.scope().clone(),
        digest,
    );
    session.driver.register_subject(&subject)?;
    session.candidates.insert(
        digest,
        ValidatorConsensusTarget::PreparedTask {
            task_id: task_id.clone(),
            plan_digest: digest,
        },
    );
    Ok(())
}
impl ValidatorConsensusRuntime {
    pub(crate) fn has_pending_prepared_commit_proof(
        &self,
        scope: &ConsensusScope,
        digests: &[[u8; 32]],
        abort_digest: [u8; 32],
        validators: &ValidatorSet,
    ) -> bool {
        let coordinator = self
            .coordinator
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        pending_for(&coordinator, scope).any(|message| match &message.message {
            BftNetworkMessage::QuorumCertificate(qc)
            | BftNetworkMessage::Proposal {
                unlock_certificate: Some(qc),
                ..
            } if qc.statement().scope() == scope
                && qc
                    .statement()
                    .value()
                    .digest()
                    .is_some_and(|digest| digest != abort_digest) =>
            {
                qc.verify(validators).is_ok()
            }
            BftNetworkMessage::FinalityCertificate {
                scope: candidate,
                certificate,
            } if candidate == scope
                && digests.contains(&certificate.statement().subject_digest()) =>
            {
                certificate.verify(validators).is_ok()
            }
            _ => false,
        })
    }

    pub(crate) fn has_certified_source(
        &self,
        scope: &ConsensusScope,
        digest: [u8; 32],
        validators: &ValidatorSet,
    ) -> bool {
        let coordinator = self
            .coordinator
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        pending_for(&coordinator, scope).any(|message| {
            match &message.message {
                BftNetworkMessage::FinalityCertificate {
                    scope: candidate,
                    certificate,
                } if candidate == scope
                    && certificate.statement().subject_digest() == digest
                    && certificate.verify(validators).is_ok() =>
                {
                    true
                }
                // A decided exact task was admitted by an honest member of the
                // original quorum. This restores time eligibility only.
                BftNetworkMessage::QuorumCertificate(qc)
                    if matches!(scope, ConsensusScope::PreparedTask(_))
                        && qc.statement().scope() == scope
                        && qc.statement().phase() == BftPhase::Precommit
                        && qc.statement().value() == BftValue::Digest(digest)
                        && qc.verify(validators).is_ok() =>
                {
                    true
                }
                _ => false,
            }
        })
    }

    pub(crate) fn pending_finality_certificate(
        &self,
        scope: &ConsensusScope,
        digest: [u8; 32],
    ) -> Option<FinalityCertificate> {
        let coordinator = self
            .coordinator
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        pending_for(&coordinator, scope).find_map(|message| match &message.message {
            BftNetworkMessage::FinalityCertificate {
                scope: candidate,
                certificate,
            } if candidate == scope && certificate.statement().subject_digest() == digest => {
                Some(certificate.clone())
            }
            _ => None,
        })
    }
    pub(crate) fn suspend_unclaimed_task(&self, scope: &ConsensusScope) {
        let mut coordinator = self
            .coordinator
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(session) = coordinator.sessions.remove(scope) {
            for message in session.pending_future {
                coordinator.buffer_unregistered(message);
            }
        }
    }
    pub(crate) fn pending_prepared_commit_qc(
        &self,
        scope: &ConsensusScope,
        digest: [u8; 32],
        validators: &ValidatorSet,
    ) -> Option<BftQuorumCertificate> {
        let coordinator = self
            .coordinator
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        pending_for(&coordinator, scope)
            .filter_map(|message| match &message.message {
                BftNetworkMessage::QuorumCertificate(qc)
                | BftNetworkMessage::Proposal {
                    unlock_certificate: Some(qc),
                    ..
                } if qc.statement().scope() == scope
                    && qc.statement().phase() == BftPhase::Prevote
                    && qc.statement().value() == BftValue::Digest(digest)
                    && qc.verify(validators).is_ok() =>
                {
                    Some(qc)
                }
                _ => None,
            })
            .max_by_key(|qc| qc.statement().round())
            .cloned()
    }
}

fn pending_for<'a>(
    coordinator: &'a BftConsensusCoordinator,
    scope: &ConsensusScope,
) -> impl Iterator<Item = &'a InboundBftMessage> {
    coordinator
        .pending_unregistered
        .get(scope)
        .into_iter()
        .flatten()
        .chain(
            coordinator
                .sessions
                .get(scope)
                .into_iter()
                .flat_map(|session| session.pending_future.iter()),
        )
}
