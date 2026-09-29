use sha2::{Digest, Sha256};

use crate::{
    CURRENT_PROTOCOL_VERSION, FinalityCertificate, FinalityStatement, ValidatorSet,
    ValidatorTransitionError, ValidatorVote,
};

const TRANSITION_DOMAIN: &[u8] = b"SECOND_VALIDATOR_SET_TRANSITION_V1\0";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatorSetTransition {
    protocol_version: u32,
    current_epoch: u64,
    activation_epoch: u64,
    current_validator_set_version: u64,
    next_validator_set: ValidatorSet,
    digest: [u8; 32],
}

impl ValidatorSetTransition {
    pub fn new(
        protocol_version: u32,
        current_epoch: u64,
        current_validator_set: &ValidatorSet,
        next_validator_set: ValidatorSet,
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

        for current in current_validator_set.credentials() {
            if let Some(next) = next_validator_set.credential(current.id())
                && next.identity_public_key() != current.identity_public_key()
            {
                return Err(ValidatorTransitionError::IdentityKeyChanged(current.id()));
            }
        }

        let activation_epoch = current_epoch
            .checked_add(1)
            .ok_or(ValidatorTransitionError::EpochOverflow)?;
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

    pub fn activate(self, epoch: u64) -> Result<ValidatorSet, ValidatorTransitionError> {
        if epoch != self.transition.activation_epoch() {
            return Err(ValidatorTransitionError::WrongActivationEpoch {
                expected: self.transition.activation_epoch(),
                actual: epoch,
            });
        }

        Ok(self.transition.next_validator_set)
    }
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
