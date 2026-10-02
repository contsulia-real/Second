use crate::validator_transition::transition_digest;
use crate::{
    CURRENT_PROTOCOL_VERSION, CertifiedValidatorSetTransition, FinalityCertificate,
    FinalityStatement, PersistenceError, ValidatorConsensusKeyRotationRequest, ValidatorId,
    ValidatorSet, ValidatorVote,
};

use super::codec::{Decoder, push_len};
use super::validator_codec::{decode_validator_set, encode_validator_set};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PendingValidatorSafetyRecovery {
    pub(crate) validator_id: ValidatorId,
    pub(crate) previous_validator_set: ValidatorSet,
    pub(crate) rotation: ValidatorConsensusKeyRotationRequest,
    pub(crate) transition_certificate: FinalityCertificate,
}

impl PendingValidatorSafetyRecovery {
    pub(crate) fn from_certified_transition(
        validator_id: ValidatorId,
        previous_validator_set: &ValidatorSet,
        certified_transition: &CertifiedValidatorSetTransition,
    ) -> Result<Option<Self>, PersistenceError> {
        let Some(previous) = previous_validator_set.validator(validator_id) else {
            return Ok(None);
        };
        let Some(current) = certified_transition
            .next_validator_set()
            .validator(validator_id)
        else {
            return Ok(None);
        };
        if previous.consensus_public_key() == current.consensus_public_key() {
            return Ok(None);
        }

        let rotation = certified_transition
            .transition()
            .consensus_key_rotations()
            .iter()
            .find(|rotation| rotation.validator_id() == validator_id)
            .ok_or(PersistenceError::InvalidValidatorSafetyRecovery)?
            .clone();

        let pending = Self {
            validator_id,
            previous_validator_set: previous_validator_set.clone(),
            rotation,
            transition_certificate: certified_transition.certificate().clone(),
        };
        pending.validate(certified_transition.next_validator_set())?;
        Ok(Some(pending))
    }

    pub(crate) fn validate(
        &self,
        current_validator_set: &ValidatorSet,
    ) -> Result<(), PersistenceError> {
        let expected_current_version = self
            .previous_validator_set
            .version()
            .checked_add(1)
            .ok_or(PersistenceError::InvalidValidatorSafetyRecovery)?;
        if current_validator_set.version() != expected_current_version {
            return Err(PersistenceError::InvalidValidatorSafetyRecovery);
        }

        let previous = self
            .previous_validator_set
            .validator(self.validator_id)
            .ok_or(PersistenceError::InvalidValidatorSafetyRecovery)?;
        let current = current_validator_set
            .validator(self.validator_id)
            .ok_or(PersistenceError::InvalidValidatorSafetyRecovery)?;

        if previous.identity_public_key() != current.identity_public_key()
            || previous.recovery_public_key() != current.recovery_public_key()
            || previous.consensus_public_key() == current.consensus_public_key()
            || current.consensus_public_key() != self.rotation.new_consensus_public_key()
            || self.rotation.validator_id() != self.validator_id
            || self.rotation.current_validator_set_version()
                != self.previous_validator_set.version()
        {
            return Err(PersistenceError::InvalidValidatorSafetyRecovery);
        }

        self.rotation.verify(previous).map_err(|error| {
            PersistenceError::ValidatorTransition(crate::ValidatorTransitionError::Rotation(error))
        })?;

        if self
            .transition_certificate
            .votes()
            .iter()
            .any(|vote| vote.validator_id() == self.validator_id)
        {
            return Err(
                PersistenceError::RecoveringValidatorVotedSafetyFenceTransition(self.validator_id),
            );
        }

        self.transition_certificate
            .verify(&self.previous_validator_set)
            .map_err(|error| {
                PersistenceError::ValidatorTransition(crate::ValidatorTransitionError::Finality(
                    error,
                ))
            })?;

        let expected_statement = FinalityStatement::new(
            CURRENT_PROTOCOL_VERSION,
            self.previous_validator_set.version(),
            transition_digest(
                CURRENT_PROTOCOL_VERSION,
                self.previous_validator_set.version(),
                current_validator_set,
            ),
        );
        if self.transition_certificate.statement() != expected_statement {
            return Err(PersistenceError::InvalidValidatorSafetyRecovery);
        }
        Ok(())
    }
}

pub(super) fn encode_optional_pending_safety_recovery(
    out: &mut Vec<u8>,
    pending: Option<&PendingValidatorSafetyRecovery>,
) -> Result<(), PersistenceError> {
    let Some(pending) = pending else {
        out.push(0);
        return Ok(());
    };

    out.push(1);
    out.extend_from_slice(&pending.validator_id.value().to_be_bytes());
    encode_validator_set(out, &pending.previous_validator_set)?;
    out.extend_from_slice(&pending.rotation.encode_bytes());

    let statement = pending.transition_certificate.statement();
    out.extend_from_slice(&statement.protocol_version().to_be_bytes());
    out.extend_from_slice(&statement.validator_set_version().to_be_bytes());
    out.extend_from_slice(&statement.subject_digest());
    push_len(out, pending.transition_certificate.votes().len())?;
    for vote in pending.transition_certificate.votes() {
        out.extend_from_slice(&vote.validator_id().value().to_be_bytes());
        out.extend_from_slice(&vote.signature_bytes());
    }
    Ok(())
}

pub(super) fn decode_optional_pending_safety_recovery(
    decoder: &mut Decoder<'_>,
) -> Result<Option<PendingValidatorSafetyRecovery>, PersistenceError> {
    match decoder.read_u8()? {
        0 => Ok(None),
        1 => {
            let validator_id = ValidatorId::new(decoder.read_u64()?);
            let previous_validator_set = decode_validator_set(decoder)?;
            let rotation =
                ValidatorConsensusKeyRotationRequest::decode_bytes(decoder.read_exact(117)?)
                    .ok_or(PersistenceError::InvalidSnapshot)?;

            let statement = FinalityStatement::new(
                decoder.read_u32()?,
                decoder.read_u64()?,
                decoder.read_array_32()?,
            );
            let vote_count = decoder.read_len()?;
            const ENCODED_VOTE_SIZE: usize = 8 + 64;
            if vote_count == 0
                || vote_count > previous_validator_set.len()
                || vote_count > decoder.remaining() / ENCODED_VOTE_SIZE
            {
                return Err(PersistenceError::InvalidSnapshot);
            }
            let mut votes = Vec::with_capacity(vote_count);
            for _ in 0..vote_count {
                votes.push(ValidatorVote::from_untrusted_parts(
                    ValidatorId::new(decoder.read_u64()?),
                    decoder.read_array_64()?,
                ));
            }

            Ok(Some(PendingValidatorSafetyRecovery {
                validator_id,
                previous_validator_set,
                rotation,
                transition_certificate: FinalityCertificate::from_untrusted_parts(statement, votes),
            }))
        }
        _ => Err(PersistenceError::InvalidSnapshot),
    }
}
