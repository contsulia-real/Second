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
) -> Result<Option<BftConsensusEvent>, BftConsensusRuntimeError> {
    let expected_statement = session.target.finality_statement(&session.validator_set);
    if scope != *session.subject.scope() || statement != expected_statement {
        return Err(BftConsensusRuntimeError::FinalityStatementMismatch);
    }
    vote.verify(&statement, &session.validator_set)
        .map_err(BftConsensusRuntimeError::Finality)?;
    let vote_id = vote.validator_id();
    session.finality_votes.insert(vote_id, vote);
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

    for vote in certificate.votes() {
        session
            .finality_votes
            .insert(vote.validator_id(), vote.clone());
    }
    let certified = session
        .target
        .certify(certificate.votes().to_vec(), &session.validator_set)?;
    session.target.persist_certified(&session.store)?;

    session.finality_certificate = Some(certificate.clone());
    schedule_finality_relay(session, now);
    relay_finality_certificate(session, certificate, output);

    if session.certified_emitted {
        return Ok(None);
    }
    session.certified_emitted = true;
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
    session.target.persist_certified(&session.store)?;

    session.finality_certificate = Some(certificate.clone());
    session.certified_emitted = true;
    relay_finality_certificate(session, certificate, output);
    Ok(Some(certified_event(certified)))
}

pub(super) fn rebroadcast_finality_votes(
    session: &BftConsensusSession,
    output: &mut BftConsensusOutput,
) {
    if let Some(certificate) = &session.finality_qc {
        output
            .outbound
            .push(BftNetworkMessage::QuorumCertificate(certificate.clone()));
    }
    if let Some(certificate) = &session.finality_certificate {
        output
            .outbound
            .push(BftNetworkMessage::FinalityCertificate {
                scope: session.subject.scope().clone(),
                certificate: certificate.clone(),
            });
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
}

pub(super) fn relay_finality_certificate(
    session: &mut BftConsensusSession,
    certificate: FinalityCertificate,
    output: &mut BftConsensusOutput,
) {
    if !session.relayed_finality_certificate {
        session.relayed_finality_certificate = true;
        output
            .outbound
            .push(BftNetworkMessage::FinalityCertificate {
                scope: session.subject.scope().clone(),
                certificate,
            });
    }
}

pub(super) fn certified_event(certified: CertifiedConsensusTarget) -> BftConsensusEvent {
    match certified {
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
