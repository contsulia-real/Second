use crate::{FinalityCertificate, FinalityStatement, ValidatorId, ValidatorVote};

const ENCODED_FINALITY_STATEMENT_SIZE: usize = 4 + 8 + 32;
pub(crate) const ENCODED_VALIDATOR_VOTE_SIZE: usize = 8 + 64;
const ENCODED_CERTIFICATE_HEADER_SIZE: usize = ENCODED_FINALITY_STATEMENT_SIZE + 4;

pub(crate) fn encode_statement_and_vote(
    statement: &FinalityStatement,
    vote: &ValidatorVote,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(ENCODED_FINALITY_STATEMENT_SIZE + ENCODED_VALIDATOR_VOTE_SIZE);
    encode_statement(&mut out, statement);
    encode_validator_vote(&mut out, vote);
    out
}

pub(crate) fn decode_statement_and_vote(
    bytes: &[u8],
) -> Option<(FinalityStatement, ValidatorVote)> {
    if bytes.len() != ENCODED_FINALITY_STATEMENT_SIZE + ENCODED_VALIDATOR_VOTE_SIZE {
        return None;
    }
    let statement = decode_statement(&bytes[..ENCODED_FINALITY_STATEMENT_SIZE])?;
    let vote = decode_validator_vote(&bytes[ENCODED_FINALITY_STATEMENT_SIZE..])?;
    Some((statement, vote))
}

pub(crate) fn encode_certificate(certificate: &FinalityCertificate) -> Option<Vec<u8>> {
    let vote_count = u32::try_from(certificate.votes().len()).ok()?;
    let vote_bytes = certificate
        .votes()
        .len()
        .checked_mul(ENCODED_VALIDATOR_VOTE_SIZE)?;
    let capacity = ENCODED_CERTIFICATE_HEADER_SIZE.checked_add(vote_bytes)?;
    let mut out = Vec::with_capacity(capacity);
    encode_statement(&mut out, &certificate.statement());
    out.extend_from_slice(&vote_count.to_be_bytes());
    for vote in certificate.votes() {
        encode_validator_vote(&mut out, vote);
    }
    Some(out)
}

pub(crate) fn decode_certificate(bytes: &[u8]) -> Option<FinalityCertificate> {
    if bytes.len() < ENCODED_CERTIFICATE_HEADER_SIZE {
        return None;
    }
    let statement = decode_statement(&bytes[..ENCODED_FINALITY_STATEMENT_SIZE])?;
    let vote_count = usize::try_from(u32::from_be_bytes(
        bytes[ENCODED_FINALITY_STATEMENT_SIZE..ENCODED_CERTIFICATE_HEADER_SIZE]
            .try_into()
            .ok()?,
    ))
    .ok()?;
    let expected = ENCODED_CERTIFICATE_HEADER_SIZE
        .checked_add(vote_count.checked_mul(ENCODED_VALIDATOR_VOTE_SIZE)?)?;
    if bytes.len() != expected {
        return None;
    }

    let (encoded_votes, remainder) =
        bytes[ENCODED_CERTIFICATE_HEADER_SIZE..].as_chunks::<ENCODED_VALIDATOR_VOTE_SIZE>();
    if !remainder.is_empty() || encoded_votes.len() != vote_count {
        return None;
    }
    let mut votes = Vec::with_capacity(vote_count);
    for encoded in encoded_votes {
        votes.push(decode_validator_vote(encoded)?);
    }
    Some(FinalityCertificate::from_untrusted_parts(statement, votes))
}

pub(crate) fn encode_validator_vote(out: &mut Vec<u8>, vote: &ValidatorVote) {
    out.extend_from_slice(&vote.validator_id().value().to_be_bytes());
    out.extend_from_slice(&vote.signature_bytes());
}

pub(crate) fn decode_validator_vote(bytes: &[u8]) -> Option<ValidatorVote> {
    if bytes.len() != ENCODED_VALIDATOR_VOTE_SIZE {
        return None;
    }
    let validator_id = ValidatorId::new(u64::from_be_bytes(bytes[..8].try_into().ok()?));
    let signature = bytes[8..].try_into().ok()?;
    Some(ValidatorVote::from_untrusted_parts(validator_id, signature))
}

fn encode_statement(out: &mut Vec<u8>, statement: &FinalityStatement) {
    out.extend_from_slice(&statement.protocol_version().to_be_bytes());
    out.extend_from_slice(&statement.validator_set_version().to_be_bytes());
    out.extend_from_slice(&statement.subject_digest());
}

fn decode_statement(bytes: &[u8]) -> Option<FinalityStatement> {
    if bytes.len() != ENCODED_FINALITY_STATEMENT_SIZE {
        return None;
    }
    Some(FinalityStatement::new(
        u32::from_be_bytes(bytes[..4].try_into().ok()?),
        u64::from_be_bytes(bytes[4..12].try_into().ok()?),
        bytes[12..].try_into().ok()?,
    ))
}
