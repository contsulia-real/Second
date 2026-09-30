use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use crate::{
    CURRENT_PROTOCOL_VERSION, FinalityCertificate, FinalityStatement,
    ValidatorConsensusKeyRotationRequest, ValidatorId, ValidatorRegistry, ValidatorSet,
    ValidatorTransitionError, ValidatorVote, VerifiedValidatorAdmission,
};

const TRANSITION_DOMAIN: &[u8] = b"SECOND_VALIDATOR_SET_TRANSITION_V1\0";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatorSetTransition {
    protocol_version: u32,
    current_epoch: u64,
    activation_epoch: u64,
    current_validator_set_version: u64,
    next_validator_set: ValidatorSet,
    admissions: Vec<VerifiedValidatorAdmission>,
    consensus_key_rotations: Vec<ValidatorConsensusKeyRotationRequest>,
    digest: [u8; 32],
}

impl ValidatorSetTransition {
    pub fn new(
        protocol_version: u32,
        current_epoch: u64,
        current_validator_set: &ValidatorSet,
        validator_registry: &ValidatorRegistry,
        next_validator_set: ValidatorSet,
        admissions: Vec<VerifiedValidatorAdmission>,
        consensus_key_rotations: Vec<ValidatorConsensusKeyRotationRequest>,
    ) -> Result<Self, ValidatorTransitionError> {
        if protocol_version != CURRENT_PROTOCOL_VERSION {
            return Err(ValidatorTransitionError::UnsupportedProtocolVersion {
                expected: CURRENT_PROTOCOL_VERSION,
                actual: protocol_version,
            });
        }

        let expected_next_version = current_validator_set
            .version()
            .checked_add(1)
            .ok_or(ValidatorTransitionError::ValidatorSetVersionOverflow)?;
        if next_validator_set.version() != expected_next_version {
            return Err(ValidatorTransitionError::WrongNextValidatorSetVersion {
                expected: expected_next_version,
                actual: next_validator_set.version(),
            });
        }

        let activation_epoch = current_epoch
            .checked_add(1)
            .ok_or(ValidatorTransitionError::EpochOverflow)?;

        validator_registry
            .validate_transition(current_validator_set, &next_validator_set)
            .map_err(map_registry_error)?;

        validate_admissions(current_validator_set, &next_validator_set, &admissions)?;
        validate_consensus_key_rotations(
            current_validator_set,
            &next_validator_set,
            activation_epoch,
            &consensus_key_rotations,
        )?;

        let current_validator_set_version = current_validator_set.version();
        let digest = transition_digest(
            protocol_version,
            current_epoch,
            activation_epoch,
            current_validator_set_version,
            &next_validator_set,
        );

        Ok(Self {
            protocol_version,
            current_epoch,
            activation_epoch,
            current_validator_set_version,
            next_validator_set,
            admissions,
            consensus_key_rotations,
            digest,
        })
    }

    pub const fn protocol_version(&self) -> u32 {
        self.protocol_version
    }

    pub const fn current_epoch(&self) -> u64 {
        self.current_epoch
    }

    pub const fn activation_epoch(&self) -> u64 {
        self.activation_epoch
    }

    pub const fn current_validator_set_version(&self) -> u64 {
        self.current_validator_set_version
    }

    pub const fn digest(&self) -> [u8; 32] {
        self.digest
    }

    pub fn next_validator_set(&self) -> &ValidatorSet {
        &self.next_validator_set
    }

    pub fn admissions(&self) -> &[VerifiedValidatorAdmission] {
        &self.admissions
    }

    pub fn consensus_key_rotations(&self) -> &[ValidatorConsensusKeyRotationRequest] {
        &self.consensus_key_rotations
    }

    pub fn finality_statement(&self) -> FinalityStatement {
        FinalityStatement::new(
            self.protocol_version,
            self.current_validator_set_version,
            self.digest,
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CertifiedValidatorSetTransition {
    transition: ValidatorSetTransition,
    certificate: FinalityCertificate,
}

impl CertifiedValidatorSetTransition {
    pub fn new(
        transition: ValidatorSetTransition,
        votes: Vec<ValidatorVote>,
        current_validator_set: &ValidatorSet,
    ) -> Result<Self, ValidatorTransitionError> {
        if transition.current_validator_set_version() != current_validator_set.version() {
            return Err(ValidatorTransitionError::WrongCurrentValidatorSetVersion {
                expected: current_validator_set.version(),
                actual: transition.current_validator_set_version(),
            });
        }

        let certificate = FinalityCertificate::new(
            transition.finality_statement(),
            votes,
            current_validator_set,
        )
        .map_err(ValidatorTransitionError::Finality)?;

        Ok(Self {
            transition,
            certificate,
        })
    }

    pub const fn activation_epoch(&self) -> u64 {
        self.transition.activation_epoch()
    }

    pub fn next_validator_set(&self) -> &ValidatorSet {
        self.transition.next_validator_set()
    }

    pub fn certificate(&self) -> &FinalityCertificate {
        &self.certificate
    }

    pub fn activate(
        self,
        epoch: u64,
        validator_registry: &mut ValidatorRegistry,
    ) -> Result<ValidatorSet, ValidatorTransitionError> {
        if epoch != self.transition.activation_epoch() {
            return Err(ValidatorTransitionError::WrongActivationEpoch {
                expected: self.transition.activation_epoch(),
                actual: epoch,
            });
        }

        validator_registry
            .apply_next_set(&self.transition.next_validator_set)
            .map_err(ValidatorTransitionError::Registry)?;

        Ok(self.transition.next_validator_set)
    }
}

fn map_registry_error(error: crate::ValidatorRegistryError) -> ValidatorTransitionError {
    match error {
        crate::ValidatorRegistryError::IdentityKeyChanged(validator_id) => {
            ValidatorTransitionError::IdentityKeyChanged(validator_id)
        }
        crate::ValidatorRegistryError::RecoveryKeyChanged(validator_id) => {
            ValidatorTransitionError::RecoveryKeyChanged(validator_id)
        }
        other => ValidatorTransitionError::Registry(other),
    }
}

fn validate_admissions(
    current_validator_set: &ValidatorSet,
    next_validator_set: &ValidatorSet,
    admissions: &[VerifiedValidatorAdmission],
) -> Result<(), ValidatorTransitionError> {
    let mut by_id = BTreeMap::<ValidatorId, &VerifiedValidatorAdmission>::new();

    for admission in admissions {
        let validator_id = admission.validator_id();
        if by_id.insert(validator_id, admission).is_some() {
            return Err(ValidatorTransitionError::DuplicateAdmission(validator_id));
        }

        if current_validator_set.contains(validator_id) {
            return Err(ValidatorTransitionError::UnexpectedAdmission(validator_id));
        }

        let next = next_validator_set
            .credential(validator_id)
            .ok_or(ValidatorTransitionError::UnexpectedAdmission(validator_id))?;

        if next != admission.credential() {
            return Err(ValidatorTransitionError::AdmissionCredentialMismatch(
                validator_id,
            ));
        }
    }

    for next in next_validator_set.credentials() {
        if !current_validator_set.contains(next.id()) && !by_id.contains_key(&next.id()) {
            return Err(ValidatorTransitionError::MissingAdmission(next.id()));
        }
    }

    Ok(())
}

fn validate_consensus_key_rotations(
    current_validator_set: &ValidatorSet,
    next_validator_set: &ValidatorSet,
    activation_epoch: u64,
    rotations: &[ValidatorConsensusKeyRotationRequest],
) -> Result<(), ValidatorTransitionError> {
    let mut by_id = BTreeMap::<ValidatorId, &ValidatorConsensusKeyRotationRequest>::new();

    for rotation in rotations {
        let validator_id = rotation.validator_id();

        if by_id.insert(validator_id, rotation).is_some() {
            return Err(ValidatorTransitionError::DuplicateConsensusKeyRotation(
                validator_id,
            ));
        }

        let current = current_validator_set.credential(validator_id).ok_or(
            ValidatorTransitionError::UnexpectedConsensusKeyRotation(validator_id),
        )?;
        let next = next_validator_set.credential(validator_id).ok_or(
            ValidatorTransitionError::UnexpectedConsensusKeyRotation(validator_id),
        )?;

        if current.consensus_public_key() == next.consensus_public_key() {
            return Err(ValidatorTransitionError::UnexpectedConsensusKeyRotation(
                validator_id,
            ));
        }

        rotation
            .verify(current)
            .map_err(ValidatorTransitionError::Rotation)?;

        if rotation.current_validator_set_version() != current_validator_set.version() {
            return Err(
                ValidatorTransitionError::ConsensusKeyRotationValidatorSetMismatch {
                    validator_id,
                    expected: current_validator_set.version(),
                    actual: rotation.current_validator_set_version(),
                },
            );
        }

        if rotation.activation_epoch() != activation_epoch {
            return Err(
                ValidatorTransitionError::ConsensusKeyRotationEpochMismatch {
                    validator_id,
                    expected: activation_epoch,
                    actual: rotation.activation_epoch(),
                },
            );
        }

        if rotation.new_consensus_public_key() != next.consensus_public_key() {
            return Err(
                ValidatorTransitionError::ConsensusKeyRotationCredentialMismatch(validator_id),
            );
        }
    }

    for current in current_validator_set.credentials() {
        let Some(next) = next_validator_set.credential(current.id()) else {
            continue;
        };

        if current.consensus_public_key() != next.consensus_public_key()
            && !by_id.contains_key(&current.id())
        {
            return Err(ValidatorTransitionError::MissingConsensusKeyRotation(
                current.id(),
            ));
        }
    }

    Ok(())
}

fn transition_digest(
    protocol_version: u32,
    current_epoch: u64,
    activation_epoch: u64,
    current_validator_set_version: u64,
    next_validator_set: &ValidatorSet,
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(TRANSITION_DOMAIN);
    hasher.update(protocol_version.to_be_bytes());
    hasher.update(current_epoch.to_be_bytes());
    hasher.update(activation_epoch.to_be_bytes());
    hasher.update(current_validator_set_version.to_be_bytes());
    hasher.update(next_validator_set.version().to_be_bytes());
    hasher.update((next_validator_set.len() as u64).to_be_bytes());

    for credential in next_validator_set.credentials() {
        hasher.update(credential.id().value().to_be_bytes());
        hasher.update(credential.identity_public_key());
        hasher.update(credential.consensus_public_key());
        hasher.update(credential.recovery_public_key());
    }

    hasher.finalize().into()
}
