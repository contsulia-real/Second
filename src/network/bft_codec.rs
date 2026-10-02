use crate::{
    BftPhase, BftProposal, BftQuorumCertificate, BftStatement, BftValue, BftVote, ConsensusScope,
    FinalityCertificate, FinalityStatement, MAX_VALIDATOR_TRANSITION_SOURCE_SIZE, TaskId,
    ValidatorId, ValidatorVote,
};

use super::{MAX_NETWORK_FRAME_SIZE, NetworkError};

const MAX_BFT_MESSAGE_SIZE: usize = MAX_NETWORK_FRAME_SIZE - 1;
pub(crate) const MAX_PREPARED_TASK_SOURCE_CHUNK_SIZE: usize = 32 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BftNetworkMessage {
    Proposal {
        proposal: BftProposal,
        unlock_certificate: Option<BftQuorumCertificate>,
    },
    Vote {
        statement: BftStatement,
        vote: BftVote,
    },
    QuorumCertificate(BftQuorumCertificate),
    FinalityVote {
        scope: ConsensusScope,
        statement: FinalityStatement,
        vote: ValidatorVote,
    },
    FinalityCertificate {
        scope: ConsensusScope,
        certificate: FinalityCertificate,
    },
    PreparedTaskRequest {
        validator_set_version: u64,
        scope: ConsensusScope,
        expected_plan_digest: [u8; 32],
        offset: u64,
    },
    PreparedTaskSourceChunk {
        validator_set_version: u64,
        scope: ConsensusScope,
        expected_plan_digest: [u8; 32],
        total_len: u64,
        offset: u64,
        bytes: Vec<u8>,
    },
    PreparedTaskSourceUnavailable {
        validator_set_version: u64,
        scope: ConsensusScope,
        expected_plan_digest: [u8; 32],
    },
    PreparedTaskAvailable {
        validator_set_version: u64,
        scope: ConsensusScope,
        expected_plan_digest: [u8; 32],
        round: u64,
    },
    ValidatorSetTransitionSource {
        validator_set_version: u64,
        scope: ConsensusScope,
        bytes: Vec<u8>,
    },
    StateRecoveryCheckpointSource {
        validator_set_version: u64,
        scope: ConsensusScope,
        bytes: [u8; 52],
    },
}

impl BftNetworkMessage {
    pub fn scope(&self) -> &ConsensusScope {
        match self {
            Self::Proposal { proposal, .. } => proposal.scope(),
            Self::Vote { statement, .. } => statement.scope(),
            Self::QuorumCertificate(certificate) => certificate.statement().scope(),
            Self::FinalityVote { scope, .. }
            | Self::FinalityCertificate { scope, .. }
            | Self::PreparedTaskRequest { scope, .. }
            | Self::PreparedTaskSourceChunk { scope, .. }
            | Self::PreparedTaskSourceUnavailable { scope, .. }
            | Self::PreparedTaskAvailable { scope, .. }
            | Self::ValidatorSetTransitionSource { scope, .. }
            | Self::StateRecoveryCheckpointSource { scope, .. } => scope,
        }
    }

    pub fn validator_set_version(&self) -> u64 {
        match self {
            Self::Proposal { proposal, .. } => proposal.validator_set_version(),
            Self::Vote { statement, .. } => statement.validator_set_version(),
            Self::QuorumCertificate(certificate) => certificate.statement().validator_set_version(),
            Self::FinalityVote { statement, .. } => statement.validator_set_version(),
            Self::FinalityCertificate { certificate, .. } => {
                certificate.statement().validator_set_version()
            }
            Self::PreparedTaskRequest {
                validator_set_version,
                ..
            }
            | Self::PreparedTaskSourceChunk {
                validator_set_version,
                ..
            }
            | Self::PreparedTaskSourceUnavailable {
                validator_set_version,
                ..
            }
            | Self::PreparedTaskAvailable {
                validator_set_version,
                ..
            }
            | Self::ValidatorSetTransitionSource {
                validator_set_version,
                ..
            }
            | Self::StateRecoveryCheckpointSource {
                validator_set_version,
                ..
            } => *validator_set_version,
        }
    }

    pub(crate) fn consensus_round(&self) -> Option<u64> {
        match self {
            Self::Proposal { proposal, .. } => Some(proposal.round()),
            Self::Vote { statement, .. } => Some(statement.round()),
            Self::QuorumCertificate(certificate) => Some(certificate.statement().round()),
            Self::FinalityVote { .. }
            | Self::FinalityCertificate { .. }
            | Self::PreparedTaskRequest { .. }
            | Self::PreparedTaskSourceChunk { .. }
            | Self::PreparedTaskSourceUnavailable { .. }
            | Self::ValidatorSetTransitionSource { .. }
            | Self::StateRecoveryCheckpointSource { .. } => None,
            Self::PreparedTaskAvailable { round, .. } => Some(*round),
        }
    }

    pub(crate) fn is_digest_precommit_evidence(&self) -> bool {
        let statement = match self {
            Self::Vote { statement, .. } => statement,
            Self::QuorumCertificate(certificate) => certificate.statement(),
            Self::Proposal { .. }
            | Self::FinalityVote { .. }
            | Self::FinalityCertificate { .. }
            | Self::PreparedTaskRequest { .. }
            | Self::PreparedTaskSourceChunk { .. }
            | Self::PreparedTaskSourceUnavailable { .. }
            | Self::PreparedTaskAvailable { .. }
            | Self::ValidatorSetTransitionSource { .. }
            | Self::StateRecoveryCheckpointSource { .. } => return false,
        };
        statement.phase() == BftPhase::Precommit && matches!(statement.value(), BftValue::Digest(_))
    }
}

pub fn encode_bft_network_message(message: &BftNetworkMessage) -> Result<Vec<u8>, NetworkError> {
    let mut out = Vec::new();
    match message {
        BftNetworkMessage::Proposal {
            proposal,
            unlock_certificate,
        } => {
            out.push(1);
            encode_proposal(proposal, &mut out)?;
            match unlock_certificate {
                Some(certificate) => {
                    out.push(1);
                    encode_qc(certificate, &mut out)?;
                }
                None => out.push(0),
            }
        }
        BftNetworkMessage::Vote { statement, vote } => {
            out.push(2);
            encode_statement(statement, &mut out)?;
            encode_vote(vote, &mut out);
        }
        BftNetworkMessage::QuorumCertificate(certificate) => {
            out.push(3);
            encode_qc(certificate, &mut out)?;
        }
        BftNetworkMessage::FinalityVote {
            scope,
            statement,
            vote,
        } => {
            out.push(4);
            encode_scope(scope, &mut out)?;
            encode_finality_statement(statement, &mut out);
            encode_finality_vote(vote, &mut out);
        }
        BftNetworkMessage::FinalityCertificate { scope, certificate } => {
            out.push(5);
            encode_scope(scope, &mut out)?;
            encode_finality_certificate(certificate, &mut out)?;
        }
        BftNetworkMessage::PreparedTaskRequest {
            validator_set_version,
            scope,
            expected_plan_digest,
            offset,
        } => {
            out.push(6);
            encode_prepared_task_control_head(
                *validator_set_version,
                scope,
                *expected_plan_digest,
                &mut out,
            )?;
            out.extend_from_slice(&offset.to_be_bytes());
        }
        BftNetworkMessage::PreparedTaskSourceChunk {
            validator_set_version,
            scope,
            expected_plan_digest,
            total_len,
            offset,
            bytes,
        } => {
            if bytes.len() > MAX_PREPARED_TASK_SOURCE_CHUNK_SIZE {
                return Err(NetworkError::InvalidBftMessage);
            }
            out.push(7);
            encode_prepared_task_control_head(
                *validator_set_version,
                scope,
                *expected_plan_digest,
                &mut out,
            )?;
            out.extend_from_slice(&total_len.to_be_bytes());
            out.extend_from_slice(&offset.to_be_bytes());
            let len = u32::try_from(bytes.len()).map_err(|_| NetworkError::InvalidBftMessage)?;
            out.extend_from_slice(&len.to_be_bytes());
            out.extend_from_slice(bytes);
        }
        BftNetworkMessage::PreparedTaskSourceUnavailable {
            validator_set_version,
            scope,
            expected_plan_digest,
        } => {
            out.push(8);
            encode_prepared_task_control_head(
                *validator_set_version,
                scope,
                *expected_plan_digest,
                &mut out,
            )?;
        }
        BftNetworkMessage::PreparedTaskAvailable {
            validator_set_version,
            scope,
            expected_plan_digest,
            round,
        } => {
            out.push(9);
            encode_prepared_task_control_head(
                *validator_set_version,
                scope,
                *expected_plan_digest,
                &mut out,
            )?;
            out.extend_from_slice(&round.to_be_bytes());
        }
        BftNetworkMessage::ValidatorSetTransitionSource {
            validator_set_version,
            scope,
            bytes,
        } => {
            if !matches!(scope, ConsensusScope::ValidatorSetTransition { .. })
                || bytes.is_empty()
                || bytes.len() > MAX_VALIDATOR_TRANSITION_SOURCE_SIZE
            {
                return Err(NetworkError::InvalidBftMessage);
            }
            out.push(10);
            out.extend_from_slice(&validator_set_version.to_be_bytes());
            encode_scope(scope, &mut out)?;
            let len = u16::try_from(bytes.len()).map_err(|_| NetworkError::InvalidBftMessage)?;
            out.extend_from_slice(&len.to_be_bytes());
            out.extend_from_slice(bytes);
        }
        BftNetworkMessage::StateRecoveryCheckpointSource {
            validator_set_version,
            scope,
            bytes,
        } => {
            if !matches!(scope, ConsensusScope::StateRecoveryCheckpoint { .. }) {
                return Err(NetworkError::InvalidBftMessage);
            }
            out.push(11);
            out.extend_from_slice(&validator_set_version.to_be_bytes());
            encode_scope(scope, &mut out)?;
            out.extend_from_slice(bytes);
        }
    }

    if out.is_empty() || out.len() > MAX_BFT_MESSAGE_SIZE {
        return Err(NetworkError::InvalidBftMessage);
    }
    Ok(out)
}

pub fn decode_bft_network_message(bytes: &[u8]) -> Result<BftNetworkMessage, NetworkError> {
    if bytes.is_empty() || bytes.len() > MAX_BFT_MESSAGE_SIZE {
        return Err(NetworkError::InvalidBftMessage);
    }
    let mut cursor = Cursor::new(bytes);
    let message = match cursor.u8()? {
        1 => {
            let proposal = decode_proposal(&mut cursor)?;
            let unlock_certificate = match cursor.u8()? {
                0 => None,
                1 => Some(decode_qc(&mut cursor)?),
                _ => return Err(NetworkError::InvalidBftMessage),
            };
            BftNetworkMessage::Proposal {
                proposal,
                unlock_certificate,
            }
        }
        2 => BftNetworkMessage::Vote {
            statement: decode_statement(&mut cursor)?,
            vote: decode_vote(&mut cursor)?,
        },
        3 => BftNetworkMessage::QuorumCertificate(decode_qc(&mut cursor)?),
        4 => BftNetworkMessage::FinalityVote {
            scope: decode_scope(&mut cursor)?,
            statement: decode_finality_statement(&mut cursor)?,
            vote: decode_finality_vote(&mut cursor)?,
        },
        5 => BftNetworkMessage::FinalityCertificate {
            scope: decode_scope(&mut cursor)?,
            certificate: decode_finality_certificate(&mut cursor)?,
        },
        6 => {
            let (validator_set_version, scope, expected_plan_digest) =
                decode_prepared_task_control_head(&mut cursor)?;
            BftNetworkMessage::PreparedTaskRequest {
                validator_set_version,
                scope,
                expected_plan_digest,
                offset: cursor.u64()?,
            }
        }
        7 => {
            let (validator_set_version, scope, expected_plan_digest) =
                decode_prepared_task_control_head(&mut cursor)?;
            let total_len = cursor.u64()?;
            let offset = cursor.u64()?;
            let len =
                usize::try_from(cursor.u32()?).map_err(|_| NetworkError::InvalidBftMessage)?;
            if len > MAX_PREPARED_TASK_SOURCE_CHUNK_SIZE || len > cursor.remaining() {
                return Err(NetworkError::InvalidBftMessage);
            }
            BftNetworkMessage::PreparedTaskSourceChunk {
                validator_set_version,
                scope,
                expected_plan_digest,
                total_len,
                offset,
                bytes: cursor.bytes(len)?.to_vec(),
            }
        }
        8 => {
            let (validator_set_version, scope, expected_plan_digest) =
                decode_prepared_task_control_head(&mut cursor)?;
            BftNetworkMessage::PreparedTaskSourceUnavailable {
                validator_set_version,
                scope,
                expected_plan_digest,
            }
        }
        9 => {
            let (validator_set_version, scope, expected_plan_digest) =
                decode_prepared_task_control_head(&mut cursor)?;
            BftNetworkMessage::PreparedTaskAvailable {
                validator_set_version,
                scope,
                expected_plan_digest,
                round: cursor.u64()?,
            }
        }
        10 => {
            let validator_set_version = cursor.u64()?;
            let scope = decode_scope(&mut cursor)?;
            if !matches!(scope, ConsensusScope::ValidatorSetTransition { .. }) {
                return Err(NetworkError::InvalidBftMessage);
            }
            let len = usize::from(cursor.u16()?);
            if len == 0 || len > MAX_VALIDATOR_TRANSITION_SOURCE_SIZE || len > cursor.remaining() {
                return Err(NetworkError::InvalidBftMessage);
            }
            BftNetworkMessage::ValidatorSetTransitionSource {
                validator_set_version,
                scope,
                bytes: cursor.bytes(len)?.to_vec(),
            }
        }
        11 => {
            let validator_set_version = cursor.u64()?;
            let scope = decode_scope(&mut cursor)?;
            if !matches!(scope, ConsensusScope::StateRecoveryCheckpoint { .. }) {
                return Err(NetworkError::InvalidBftMessage);
            }
            BftNetworkMessage::StateRecoveryCheckpointSource {
                validator_set_version,
                scope,
                bytes: cursor.array::<52>()?,
            }
        }
        _ => return Err(NetworkError::InvalidBftMessage),
    };
    if !cursor.finished() {
        return Err(NetworkError::InvalidBftMessage);
    }
    Ok(message)
}

fn encode_prepared_task_control_head(
    validator_set_version: u64,
    scope: &ConsensusScope,
    expected_plan_digest: [u8; 32],
    out: &mut Vec<u8>,
) -> Result<(), NetworkError> {
    if !matches!(scope, ConsensusScope::PreparedTask(_)) {
        return Err(NetworkError::InvalidBftMessage);
    }
    out.extend_from_slice(&validator_set_version.to_be_bytes());
    encode_scope(scope, out)?;
    out.extend_from_slice(&expected_plan_digest);
    Ok(())
}

fn decode_prepared_task_control_head(
    cursor: &mut Cursor<'_>,
) -> Result<(u64, ConsensusScope, [u8; 32]), NetworkError> {
    let validator_set_version = cursor.u64()?;
    let scope = decode_scope(cursor)?;
    if !matches!(scope, ConsensusScope::PreparedTask(_)) {
        return Err(NetworkError::InvalidBftMessage);
    }
    let expected_plan_digest = cursor.array::<32>()?;
    Ok((validator_set_version, scope, expected_plan_digest))
}

fn encode_proposal(proposal: &BftProposal, out: &mut Vec<u8>) -> Result<(), NetworkError> {
    out.extend_from_slice(&proposal.protocol_version().to_be_bytes());
    out.extend_from_slice(&proposal.validator_set_version().to_be_bytes());
    encode_scope(proposal.scope(), out)?;
    out.extend_from_slice(&proposal.round().to_be_bytes());
    out.extend_from_slice(&proposal.proposer_id().value().to_be_bytes());
    out.extend_from_slice(&proposal.subject_digest());
    out.extend_from_slice(&proposal.signature_bytes());
    Ok(())
}

fn decode_proposal(cursor: &mut Cursor<'_>) -> Result<BftProposal, NetworkError> {
    Ok(BftProposal::from_untrusted_parts(
        cursor.u32()?,
        cursor.u64()?,
        decode_scope(cursor)?,
        cursor.u64()?,
        ValidatorId::new(cursor.u64()?),
        cursor.array::<32>()?,
        cursor.array::<64>()?,
    ))
}

fn encode_qc(certificate: &BftQuorumCertificate, out: &mut Vec<u8>) -> Result<(), NetworkError> {
    encode_statement(certificate.statement(), out)?;
    let count =
        u16::try_from(certificate.votes().len()).map_err(|_| NetworkError::InvalidBftMessage)?;
    out.extend_from_slice(&count.to_be_bytes());
    for vote in certificate.votes() {
        encode_vote(vote, out);
    }
    Ok(())
}

fn decode_qc(cursor: &mut Cursor<'_>) -> Result<BftQuorumCertificate, NetworkError> {
    let statement = decode_statement(cursor)?;
    let count = usize::from(cursor.u16()?);
    const ENCODED_VOTE_SIZE: usize = 8 + 64;
    if count > cursor.remaining() / ENCODED_VOTE_SIZE {
        return Err(NetworkError::InvalidBftMessage);
    }
    let mut votes = Vec::with_capacity(count);
    for _ in 0..count {
        votes.push(decode_vote(cursor)?);
    }
    Ok(BftQuorumCertificate::from_untrusted_parts(statement, votes))
}

fn encode_statement(statement: &BftStatement, out: &mut Vec<u8>) -> Result<(), NetworkError> {
    out.extend_from_slice(&statement.protocol_version().to_be_bytes());
    out.extend_from_slice(&statement.validator_set_version().to_be_bytes());
    encode_scope(statement.scope(), out)?;
    out.extend_from_slice(&statement.round().to_be_bytes());
    out.push(match statement.phase() {
        BftPhase::Prevote => 1,
        BftPhase::Precommit => 2,
    });
    encode_value(statement.value(), out);
    Ok(())
}

fn decode_statement(cursor: &mut Cursor<'_>) -> Result<BftStatement, NetworkError> {
    let protocol_version = cursor.u32()?;
    let validator_set_version = cursor.u64()?;
    let scope = decode_scope(cursor)?;
    let round = cursor.u64()?;
    let phase = match cursor.u8()? {
        1 => BftPhase::Prevote,
        2 => BftPhase::Precommit,
        _ => return Err(NetworkError::InvalidBftMessage),
    };
    let value = decode_value(cursor)?;
    Ok(BftStatement::from_untrusted_parts(
        protocol_version,
        validator_set_version,
        scope,
        round,
        phase,
        value,
    ))
}

fn encode_vote(vote: &BftVote, out: &mut Vec<u8>) {
    out.extend_from_slice(&vote.validator_id().value().to_be_bytes());
    out.extend_from_slice(&vote.signature_bytes());
}

fn decode_vote(cursor: &mut Cursor<'_>) -> Result<BftVote, NetworkError> {
    Ok(BftVote::from_untrusted_parts(
        ValidatorId::new(cursor.u64()?),
        cursor.array::<64>()?,
    ))
}

fn encode_finality_statement(statement: &FinalityStatement, out: &mut Vec<u8>) {
    out.extend_from_slice(&statement.protocol_version().to_be_bytes());
    out.extend_from_slice(&statement.validator_set_version().to_be_bytes());
    out.extend_from_slice(&statement.subject_digest());
}

fn decode_finality_statement(cursor: &mut Cursor<'_>) -> Result<FinalityStatement, NetworkError> {
    Ok(FinalityStatement::new(
        cursor.u32()?,
        cursor.u64()?,
        cursor.array::<32>()?,
    ))
}

fn encode_finality_certificate(
    certificate: &FinalityCertificate,
    out: &mut Vec<u8>,
) -> Result<(), NetworkError> {
    encode_finality_statement(&certificate.statement(), out);
    let count =
        u16::try_from(certificate.votes().len()).map_err(|_| NetworkError::InvalidBftMessage)?;
    out.extend_from_slice(&count.to_be_bytes());
    for vote in certificate.votes() {
        encode_finality_vote(vote, out);
    }
    Ok(())
}

fn decode_finality_certificate(
    cursor: &mut Cursor<'_>,
) -> Result<FinalityCertificate, NetworkError> {
    let statement = decode_finality_statement(cursor)?;
    let count = usize::from(cursor.u16()?);
    const ENCODED_FINALITY_VOTE_SIZE: usize = 8 + 64;
    if count > cursor.remaining() / ENCODED_FINALITY_VOTE_SIZE {
        return Err(NetworkError::InvalidBftMessage);
    }
    let mut votes = Vec::with_capacity(count);
    for _ in 0..count {
        votes.push(decode_finality_vote(cursor)?);
    }
    Ok(FinalityCertificate::from_untrusted_parts(statement, votes))
}

fn encode_finality_vote(vote: &ValidatorVote, out: &mut Vec<u8>) {
    out.extend_from_slice(&vote.validator_id().value().to_be_bytes());
    out.extend_from_slice(&vote.signature_bytes());
}

fn decode_finality_vote(cursor: &mut Cursor<'_>) -> Result<ValidatorVote, NetworkError> {
    Ok(ValidatorVote::from_untrusted_parts(
        ValidatorId::new(cursor.u64()?),
        cursor.array::<64>()?,
    ))
}

fn encode_scope(scope: &ConsensusScope, out: &mut Vec<u8>) -> Result<(), NetworkError> {
    match scope {
        ConsensusScope::PreparedTask(task_id) => {
            let len = u8::try_from(task_id.len()).map_err(|_| NetworkError::InvalidBftMessage)?;
            out.push(1);
            out.push(len);
            out.extend_from_slice(task_id.as_bytes());
        }
        ConsensusScope::PublicCheckpoint {
            validator_set_version,
            epoch,
        } => {
            out.push(2);
            out.extend_from_slice(&validator_set_version.to_be_bytes());
            out.extend_from_slice(&epoch.to_be_bytes());
        }
        ConsensusScope::ValidatorSetTransition {
            current_validator_set_version,
        } => {
            out.push(3);
            out.extend_from_slice(&current_validator_set_version.to_be_bytes());
        }
        ConsensusScope::StateRecoveryCheckpoint {
            validator_set_version,
            serial,
        } => {
            out.push(4);
            out.extend_from_slice(&validator_set_version.to_be_bytes());
            out.extend_from_slice(&serial.to_be_bytes());
        }
    }
    Ok(())
}

fn decode_scope(cursor: &mut Cursor<'_>) -> Result<ConsensusScope, NetworkError> {
    match cursor.u8()? {
        1 => {
            let len = usize::from(cursor.u8()?);
            let bytes = cursor.bytes(len)?;
            let task_id =
                TaskId::from_ascii_bytes(bytes).map_err(|_| NetworkError::InvalidBftMessage)?;
            Ok(ConsensusScope::PreparedTask(task_id))
        }
        2 => Ok(ConsensusScope::PublicCheckpoint {
            validator_set_version: cursor.u64()?,
            epoch: cursor.u64()?,
        }),
        3 => Ok(ConsensusScope::ValidatorSetTransition {
            current_validator_set_version: cursor.u64()?,
        }),
        4 => Ok(ConsensusScope::StateRecoveryCheckpoint {
            validator_set_version: cursor.u64()?,
            serial: cursor.u64()?,
        }),
        _ => Err(NetworkError::InvalidBftMessage),
    }
}

fn encode_value(value: BftValue, out: &mut Vec<u8>) {
    match value {
        BftValue::Nil => out.push(0),
        BftValue::Digest(digest) => {
            out.push(1);
            out.extend_from_slice(&digest);
        }
    }
}

fn decode_value(cursor: &mut Cursor<'_>) -> Result<BftValue, NetworkError> {
    match cursor.u8()? {
        0 => Ok(BftValue::Nil),
        1 => Ok(BftValue::Digest(cursor.array::<32>()?)),
        _ => Err(NetworkError::InvalidBftMessage),
    }
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn u8(&mut self) -> Result<u8, NetworkError> {
        Ok(self.array::<1>()?[0])
    }

    fn u16(&mut self) -> Result<u16, NetworkError> {
        Ok(u16::from_be_bytes(self.array::<2>()?))
    }

    fn u32(&mut self) -> Result<u32, NetworkError> {
        Ok(u32::from_be_bytes(self.array::<4>()?))
    }

    fn u64(&mut self) -> Result<u64, NetworkError> {
        Ok(u64::from_be_bytes(self.array::<8>()?))
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], NetworkError> {
        let end = self
            .offset
            .checked_add(N)
            .ok_or(NetworkError::InvalidBftMessage)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(NetworkError::InvalidBftMessage)?
            .try_into()
            .map_err(|_| NetworkError::InvalidBftMessage)?;
        self.offset = end;
        Ok(value)
    }

    fn bytes(&mut self, len: usize) -> Result<&'a [u8], NetworkError> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or(NetworkError::InvalidBftMessage)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(NetworkError::InvalidBftMessage)?;
        self.offset = end;
        Ok(value)
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.offset
    }

    fn finished(&self) -> bool {
        self.offset == self.bytes.len()
    }
}
