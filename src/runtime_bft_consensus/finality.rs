use crate::runtime_consensus_target::ValidatorConsensusTarget;
use tokio::time::Instant;

use super::{
    BftConsensusEvent, BftConsensusOutput, BftConsensusRuntimeError, BftConsensusSession,
    BftNetworkMessage, BftPhase, BftQuorumCertificate, BftValue, CertifiedConsensusTarget,
    ConsensusScope, FinalityCertificate, ValidatorVote, add_duration,
};

pub(super) fn begin_business_finality(
    session: &mut BftConsensusSession,
    output: &mut BftConsensusOutput,
    now: Instant,
) -> Result<Option<BftConsensusEvent>, BftConsensusRuntimeError> {
    session.bft_finality_ready = true;
    schedule_finality_relay(session, now);

    let statement = session.target.finality_statement(&session.validator_set);
    if statement.subject_digest() != session.subject.digest() {
        return Err(BftConsensusRuntimeError::FinalityStatementMismatch);
    }

    let vote = session
        .target
        .sign_vote(&session.signer, &session.validator_set)?;
    let validator_id = vote.validator_id();
    session.finality_votes.insert(validator_id, vote.clone());
    output.outbound.push(BftNetworkMessage::FinalityVote {
        scope: session.subject.scope().clone(),
        statement,
        vote,
    });
    certify_if_ready(session, output)
}

pub(super) fn ingest_finality_vote(
    session: &mut BftConsensusSession,
    scope: ConsensusScope,
    statement: crate::FinalityStatement,
    vote: ValidatorVote,
    output: &mut BftConsensusOutput,
    now: Instant,
) -> Result<Option<BftConsensusEvent>, BftConsensusRuntimeError> {
    let expected_statement = session.target.finality_statement(&session.validator_set);
    if scope != *session.subject.scope() || statement != expected_statement {
        return Err(BftConsensusRuntimeError::FinalityStatementMismatch);
    }
    vote.verify(&statement, &session.validator_set)
        .map_err(BftConsensusRuntimeError::Finality)?;
    let vote_id = vote.validator_id();
    session.finality_votes.insert(vote_id, vote);
    if session.finality_votes.len() >= session.validator_set.quorum_threshold() {
        session.bft_finality_ready = true;
        schedule_finality_relay(session, now);
    }
    certify_if_ready(session, output)
}

pub(super) fn ingest_finality_certificate(
    session: &mut BftConsensusSession,
    scope: ConsensusScope,
    certificate: FinalityCertificate,
    output: &mut BftConsensusOutput,
    now: Instant,
) -> Result<Option<BftConsensusEvent>, BftConsensusRuntimeError> {
    let expected_statement = session.target.finality_statement(&session.validator_set);
    if scope != *session.subject.scope() || certificate.statement() != expected_statement {
        return Err(BftConsensusRuntimeError::FinalityStatementMismatch);
    }
    certificate
        .verify(&session.validator_set)
        .map_err(BftConsensusRuntimeError::Finality)?;
    if session.certified_emitted {
        return Ok(None);
    }

    for vote in certificate.votes() {
        session
            .finality_votes
            .insert(vote.validator_id(), vote.clone());
    }
    session.bft_finality_ready = true;
    schedule_finality_relay(session, now);
    let certified = session
        .target
        .certify(certificate.votes().to_vec(), &session.validator_set)?;
    persist_certified_and_schedule_retry(session, &certificate, output)?;
    apply_certified_side_effects(session, &certified, output)?;
    remember_recovery_provider(&certified, output);

    session.finality_certificate = Some(certificate.clone());
    relay_finality_certificate(session, certificate, output)?;
    session.certified_emitted = true;
    session.finished = true;
    session.deadline = None;
    Ok(Some(certified_event(certified)))
}

pub(super) fn certify_if_ready(
    session: &mut BftConsensusSession,
    output: &mut BftConsensusOutput,
) -> Result<Option<BftConsensusEvent>, BftConsensusRuntimeError> {
    if session.certified_emitted
        || session.finality_votes.len() < session.validator_set.quorum_threshold()
    {
        return Ok(None);
    }

    let statement = session.target.finality_statement(&session.validator_set);
    let votes = session.finality_votes.values().cloned().collect::<Vec<_>>();
    let certificate = FinalityCertificate::new(statement, votes.clone(), &session.validator_set)
        .map_err(BftConsensusRuntimeError::Finality)?;
    let certified = session.target.certify(votes, &session.validator_set)?;
    persist_certified_and_schedule_retry(session, &certificate, output)?;
    apply_certified_side_effects(session, &certified, output)?;
    remember_recovery_provider(&certified, output);

    session.finality_certificate = Some(certificate.clone());
    session.certified_emitted = true;
    relay_finality_certificate(session, certificate, output)?;
    session.finished = true;
    session.deadline = None;
    Ok(Some(certified_event(certified)))
}

fn persist_certified_and_schedule_retry(
    session: &BftConsensusSession,
    certificate: &FinalityCertificate,
    output: &mut BftConsensusOutput,
) -> Result<(), BftConsensusRuntimeError> {
    // Commit releases unused retained candidates as well as Abort releasing all
    // claims. Collect affected contenders while their holder still exists.
    let retries = if let ValidatorConsensusTarget::PreparedTask { task_id, .. } = &session.target {
        session
            .store
            .contenders_for(task_id)
            .map_err(BftConsensusRuntimeError::Persistence)?
    } else {
        Vec::new()
    };
    session
        .target
        .persist_certified(&session.store, certificate)?;
    output.retry_prepared_tasks.extend(retries);
    Ok(())
}

fn remember_recovery_provider(
    certified: &CertifiedConsensusTarget,
    output: &mut BftConsensusOutput,
) {
    if let CertifiedConsensusTarget::StateRecoveryCheckpoint(checkpoint) = certified {
        output.certified_recovery_checkpoint = Some(checkpoint.clone());
    }
}

fn apply_certified_side_effects(
    session: &BftConsensusSession,
    certified: &CertifiedConsensusTarget,
    output: &mut BftConsensusOutput,
) -> Result<(), BftConsensusRuntimeError> {
    match certified {
        CertifiedConsensusTarget::CurrencyAllocation { .. } => {
            output.currency_allocation_committed = true;
        }
        CertifiedConsensusTarget::PreparedTask { task_id, .. } => {
            let snapshot = session
                .store
                .load_shared()
                .map_err(BftConsensusRuntimeError::Persistence)?
                .ok_or(BftConsensusRuntimeError::Persistence(
                    crate::PersistenceError::MissingSnapshot,
                ))?;
            if snapshot.state.task_cancelled(task_id.clone()) {
                output.business_state_changed = true;
                output.completed_prepared_tasks.push(task_id.clone());
            } else {
                // Finality is already verified and durable; execute its whole
                // component once and notify every task whose resources released.
                let outcome = crate::PreparedTaskBook::commit_certified_component(
                    &session.store,
                    task_id,
                    None,
                )
                .map_err(BftConsensusRuntimeError::Preparation)?;
                output.business_state_changed |= !outcome.completed.is_empty();
                output.completed_prepared_tasks.extend(outcome.completed);
                output.retry_prepared_tasks.extend(outcome.retry);
            }
        }
        CertifiedConsensusTarget::ValidatorSetTransition(certified) => {
            session
                .store
                .activate_validator_set_transition_for_runtime(
                    certified,
                    session.signer.validator_id(),
                )
                .map_err(BftConsensusRuntimeError::Persistence)?;
            output.validator_set_changed = true;
        }
        CertifiedConsensusTarget::StateRecoveryCheckpoint(certified) => {
            session
                .store
                .install_recovery_checkpoint_evidence(certified)
                .map_err(BftConsensusRuntimeError::Persistence)?;
            session
                .store
                .try_complete_pending_validator_safety_recovery(
                    session.signer.validator_id(),
                    session.signer.consensus_public_key(),
                )
                .map_err(BftConsensusRuntimeError::Persistence)?;
        }
        CertifiedConsensusTarget::PublicCheckpoint(checkpoint) => {
            session
                .store
                .install_public_checkpoint_evidence(checkpoint)
                .map_err(BftConsensusRuntimeError::Persistence)?;
        }
    }
    Ok(())
}

pub(super) fn rebroadcast_finality_votes(
    session: &BftConsensusSession,
    output: &mut BftConsensusOutput,
) -> Result<(), BftConsensusRuntimeError> {
    if let Some(certificate) = &session.finality_qc {
        output
            .outbound
            .push(BftNetworkMessage::QuorumCertificate(certificate.clone()));
    }
    if let Some(certificate) = &session.finality_certificate {
        output
            .outbound
            .push(finality_message(session, certificate)?);
    }

    if session.finality_certificate.is_none()
        && let Some(vote) = session.finality_votes.get(&session.signer.validator_id())
    {
        output.outbound.push(BftNetworkMessage::FinalityVote {
            scope: session.subject.scope().clone(),
            statement: session.target.finality_statement(&session.validator_set),
            vote: vote.clone(),
        });
    }
    Ok(())
}

pub(super) fn relay_finality_certificate(
    session: &mut BftConsensusSession,
    certificate: FinalityCertificate,
    output: &mut BftConsensusOutput,
) -> Result<(), BftConsensusRuntimeError> {
    if !session.relayed_finality_certificate {
        let message = finality_message(session, &certificate)?;
        session.relayed_finality_certificate = true;
        output.outbound.push(message);
    }
    Ok(())
}

fn finality_message(
    session: &BftConsensusSession,
    certificate: &FinalityCertificate,
) -> Result<BftNetworkMessage, BftConsensusRuntimeError> {
    if let ValidatorConsensusTarget::StateRecoveryCheckpoint(checkpoint) = &session.target {
        return super::governance::recovery_proof_message(checkpoint, certificate);
    }
    Ok(BftNetworkMessage::FinalityCertificate {
        scope: session.subject.scope().clone(),
        certificate: certificate.clone(),
    })
}

pub(super) fn certified_event(certified: CertifiedConsensusTarget) -> BftConsensusEvent {
    match certified {
        CertifiedConsensusTarget::CurrencyAllocation {
            allocation,
            certificate,
        } => BftConsensusEvent::CertifiedCurrencyAllocation {
            allocation,
            certificate,
        },
        CertifiedConsensusTarget::PreparedTask {
            task_id,
            certificate,
        } => BftConsensusEvent::CertifiedPreparedTask {
            task_id,
            certificate,
        },
        CertifiedConsensusTarget::PublicCheckpoint(value) => {
            BftConsensusEvent::CertifiedPublicCheckpoint(value)
        }
        CertifiedConsensusTarget::ValidatorSetTransition(value) => {
            BftConsensusEvent::CertifiedValidatorSetTransition(value)
        }
        CertifiedConsensusTarget::StateRecoveryCheckpoint(value) => {
            BftConsensusEvent::CertifiedStateRecoveryCheckpoint(value)
        }
    }
}

pub(super) fn remember_finality_qc(
    session: &mut BftConsensusSession,
    certificate: &BftQuorumCertificate,
) {
    let statement = certificate.statement();
    if statement.phase() == BftPhase::Precommit
        && statement.value() == BftValue::Digest(session.subject.digest())
    {
        session.finality_qc = Some(certificate.clone());
    }
}

pub(super) fn schedule_finality_relay(session: &mut BftConsensusSession, now: Instant) {
    session.deadline = Some(add_duration(now, session.timeouts.precommit));
}
