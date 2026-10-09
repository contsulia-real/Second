use crate::validator_transition::transition_digest;
use crate::{
    CURRENT_PROTOCOL_VERSION, CertifiedValidatorSetTransition, FinalityCertificate,
    FinalityStatement, PersistenceError, ValidatorConsensusKeyRotationRequest, ValidatorId,
    ValidatorSet,
};

use super::codec::{Decoder, push_len};
use super::validator_codec::{decode_validator_set, encode_validator_set};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PendingValidatorSafetyRecovery {
    pub(crate) validator_id: ValidatorId,
    pub(crate) currency_frontier: u64,
    pub(crate) previous_validator_set: ValidatorSet,
    pub(crate) rotation: ValidatorConsensusKeyRotationRequest,
    pub(crate) transition_certificate: FinalityCertificate,
    pub(crate) handoff_digest: Option<[u8; 32]>,
}

impl PendingValidatorSafetyRecovery {
    pub(crate) fn validate_current(
        &self,
        current: &ValidatorSet,
        proofs: &std::collections::BTreeMap<u64, crate::ValidatorSetTransitionProof>,
    ) -> Result<(), PersistenceError> {
        let proof = proofs
            .get(&self.previous_validator_set.version())
            .ok_or(PersistenceError::InvalidValidatorSafetyRecovery)?;
        let rotated = proof.source().next_validator_set();
        self.validate(rotated)?;
        if proof.votes() != self.transition_certificate.votes()
            || proof.source().currency_frontier() != self.currency_frontier
            || current.version() < rotated.version()
            || current.credential(self.validator_id) != rotated.credential(self.validator_id)
        {
            return Err(PersistenceError::InvalidValidatorSafetyRecovery);
        }
        Ok(())
    }

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
            currency_frontier: certified_transition.transition().currency_frontier(),
            previous_validator_set: previous_validator_set.clone(),
            rotation,
            transition_certificate: certified_transition.certificate().clone(),
            handoff_digest: certified_transition.transition().handoff_digest,
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
                self.currency_frontier,
                self.handoff_digest,
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
    out.extend_from_slice(&pending.currency_frontier.to_be_bytes());
    out.push(u8::from(pending.handoff_digest.is_some()));
    if let Some(digest) = pending.handoff_digest {
        out.extend_from_slice(&digest);
    }
    encode_validator_set(out, &pending.previous_validator_set)?;
    out.extend_from_slice(&pending.rotation.encode_bytes());

    let certificate = crate::finality_codec::encode_certificate(&pending.transition_certificate)
        .ok_or(PersistenceError::SnapshotTooLarge)?;
    push_len(out, certificate.len())?;
    out.extend_from_slice(&certificate);
    Ok(())
}

pub(super) fn decode_optional_pending_safety_recovery(
    decoder: &mut Decoder<'_>,
) -> Result<Option<PendingValidatorSafetyRecovery>, PersistenceError> {
    match decoder.read_u8()? {
        0 => Ok(None),
        1 => {
            let validator_id = ValidatorId::new(decoder.read_u64()?);
            let currency_frontier = decoder.read_u64()?;
            let handoff_digest = match decoder.read_u8()? {
                0 => None,
                1 => Some(decoder.read_array_32()?),
                _ => return Err(PersistenceError::InvalidSnapshot),
            };
            let previous_validator_set = decode_validator_set(decoder)?;
            let rotation =
                ValidatorConsensusKeyRotationRequest::decode_bytes(decoder.read_exact(117)?)
                    .ok_or(PersistenceError::InvalidSnapshot)?;

            let certificate_len = decoder.read_len()?;
            let transition_certificate =
                crate::finality_codec::decode_certificate(decoder.read_exact(certificate_len)?)
                    .ok_or(PersistenceError::InvalidSnapshot)?;
            if transition_certificate.votes().is_empty()
                || transition_certificate.votes().len() > previous_validator_set.len()
            {
                return Err(PersistenceError::InvalidSnapshot);
            }

            Ok(Some(PendingValidatorSafetyRecovery {
                validator_id,
                currency_frontier,
                previous_validator_set,
                rotation,
                transition_certificate,
                handoff_digest,
            }))
        }
        _ => Err(PersistenceError::InvalidSnapshot),
    }
}
