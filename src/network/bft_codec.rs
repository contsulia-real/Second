use crate::{
    BftPhase, BftProposal, BftQuorumCertificate, BftStatement, BftValue, BftVote, ConsensusScope,
    TaskId, ValidatorId,
};

use super::{MAX_NETWORK_FRAME_SIZE, NetworkError};

const MAX_BFT_MESSAGE_SIZE: usize = MAX_NETWORK_FRAME_SIZE - 1;

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
        _ => return Err(NetworkError::InvalidBftMessage),
    };
    if !cursor.finished() {
        return Err(NetworkError::InvalidBftMessage);
    }
    Ok(message)
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
