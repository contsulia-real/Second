use sha2::{Digest, Sha256};

use crate::{
    FinalityCertificate, FinalityError, FinalityStatement, PublicCurrencySummary,
    PublicCurrencyView, ValidatorSet, ValidatorVote,
};

const PUBLIC_CHECKPOINT_DOMAIN: &[u8] = b"SECOND_PUBLIC_CURRENCY_CHECKPOINT_V1\0";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PublicCheckpointError {
    SummaryMismatch,
    StatementMismatch,
    Finality(FinalityError),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicCurrencyCheckpoint {
    protocol_version: u32,
    epoch: u64,
    summary: PublicCurrencySummary,
}

impl PublicCurrencyCheckpoint {
    pub fn new(protocol_version: u32, epoch: u64, summary: PublicCurrencySummary) -> Self {
        Self {
            protocol_version,
            epoch,
            summary,
        }
    }

    pub const fn protocol_version(&self) -> u32 {
        self.protocol_version
    }

    pub const fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn summary(&self) -> &PublicCurrencySummary {
        &self.summary
    }

    pub fn digest(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(PUBLIC_CHECKPOINT_DOMAIN);
        hasher.update(self.protocol_version.to_be_bytes());
        hasher.update(self.epoch.to_be_bytes());
        hasher.update(self.summary.next_currency_address.to_be_bytes());
        hasher.update(self.summary.current_supply.to_be_bytes());
        hasher.update(self.summary.reserve_count.to_be_bytes());
        hasher.update(self.summary.occupied_count.to_be_bytes());
        hasher.update(self.summary.state_digest);
        hasher.finalize().into()
    }

    pub fn finality_statement(&self, validator_set_version: u64) -> FinalityStatement {
        FinalityStatement::new(self.protocol_version, validator_set_version, self.digest())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicCurrencyCheckpointProof {
    checkpoint: PublicCurrencyCheckpoint,
    validator_set_version: u64,
    votes: Vec<ValidatorVote>,
}

impl PublicCurrencyCheckpointProof {
    pub fn new(
        checkpoint: PublicCurrencyCheckpoint,
        validator_set_version: u64,
        votes: Vec<ValidatorVote>,
    ) -> Self {
        Self {
            checkpoint,
            validator_set_version,
            votes,
        }
    }

    pub fn checkpoint(&self) -> &PublicCurrencyCheckpoint {
        &self.checkpoint
    }

    pub const fn validator_set_version(&self) -> u64 {
        self.validator_set_version
    }

    pub fn votes(&self) -> &[ValidatorVote] {
        &self.votes
    }

    pub fn verify(
        self,
        view: &PublicCurrencyView,
        validator_set: &ValidatorSet,
    ) -> Result<CertifiedPublicCurrencyCheckpoint, PublicCheckpointError> {
        if self.checkpoint.summary() != &view.summary {
            return Err(PublicCheckpointError::SummaryMismatch);
        }

        let certificate = FinalityCertificate::new(
            self.checkpoint
                .finality_statement(self.validator_set_version),
            self.votes,
            validator_set,
        )
        .map_err(PublicCheckpointError::Finality)?;

        Ok(CertifiedPublicCurrencyCheckpoint {
            checkpoint: self.checkpoint,
            certificate,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CertifiedPublicCurrencyCheckpoint {
    checkpoint: PublicCurrencyCheckpoint,
    certificate: FinalityCertificate,
}

impl CertifiedPublicCurrencyCheckpoint {
    pub fn new(
        checkpoint: PublicCurrencyCheckpoint,
        votes: Vec<ValidatorVote>,
        validator_set: &ValidatorSet,
    ) -> Result<Self, FinalityError> {
        let certificate = FinalityCertificate::new(
            checkpoint.finality_statement(validator_set.version()),
            votes,
            validator_set,
        )?;

        Ok(Self {
            checkpoint,
            certificate,
        })
    }

    pub fn checkpoint(&self) -> &PublicCurrencyCheckpoint {
        &self.checkpoint
    }

    pub fn certificate(&self) -> &FinalityCertificate {
        &self.certificate
    }

    pub fn verify_view(
        &self,
        view: &PublicCurrencyView,
        validator_set: &ValidatorSet,
    ) -> Result<(), PublicCheckpointError> {
        if self.checkpoint.summary() != &view.summary {
            return Err(PublicCheckpointError::SummaryMismatch);
        }

        let statement = self.certificate.statement();
        if statement.protocol_version() != self.checkpoint.protocol_version()
            || statement.subject_digest() != self.checkpoint.digest()
        {
            return Err(PublicCheckpointError::StatementMismatch);
        }

        self.certificate
            .verify(validator_set)
            .map_err(PublicCheckpointError::Finality)
    }
}
