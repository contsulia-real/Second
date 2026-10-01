use sha2::{Digest, Sha256};

use crate::{
    FinalityCertificate, FinalityError, FinalityStatement, PublicCurrencySummary,
    PublicCurrencyView, ValidatorSet, ValidatorVote,
};

const PUBLIC_CHECKPOINT_DOMAIN: &[u8] = b"SECOND_PUBLIC_CURRENCY_CHECKPOINT_V1\0";
const CHECKPOINT_PROOF_FIXED_SIZE: usize = 92;
const CHECKPOINT_VOTE_ENCODED_SIZE: usize = 72;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CheckpointProofCodecError {
    LengthOverflow,
    InvalidLength,
}

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

// Transport/storage form only. Receiving or decoding this type does not authenticate it.
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

    pub(crate) fn encode_bytes(&self) -> Result<Vec<u8>, CheckpointProofCodecError> {
        let encoded_len = self
            .votes
            .len()
            .checked_mul(CHECKPOINT_VOTE_ENCODED_SIZE)
            .and_then(|len| CHECKPOINT_PROOF_FIXED_SIZE.checked_add(len))
            .ok_or(CheckpointProofCodecError::LengthOverflow)?;
        let vote_count = u64::try_from(self.votes.len())
            .map_err(|_| CheckpointProofCodecError::LengthOverflow)?;

        let checkpoint = self.checkpoint();
        let summary = checkpoint.summary();
        let mut bytes = Vec::with_capacity(encoded_len);
        bytes.extend_from_slice(&checkpoint.protocol_version().to_be_bytes());
        bytes.extend_from_slice(&checkpoint.epoch().to_be_bytes());
        bytes.extend_from_slice(&summary.next_currency_address.to_be_bytes());
        bytes.extend_from_slice(&summary.current_supply.to_be_bytes());
        bytes.extend_from_slice(&summary.reserve_count.to_be_bytes());
        bytes.extend_from_slice(&summary.occupied_count.to_be_bytes());
        bytes.extend_from_slice(&summary.state_digest);
        bytes.extend_from_slice(&self.validator_set_version.to_be_bytes());
        bytes.extend_from_slice(&vote_count.to_be_bytes());

        for vote in &self.votes {
            bytes.extend_from_slice(&vote.validator_id().value().to_be_bytes());
            bytes.extend_from_slice(&vote.signature_bytes());
        }

        Ok(bytes)
    }

    pub(crate) fn decode_bytes(bytes: &[u8]) -> Result<Self, CheckpointProofCodecError> {
        if bytes.len() < CHECKPOINT_PROOF_FIXED_SIZE {
            return Err(CheckpointProofCodecError::InvalidLength);
        }

        let protocol_version = u32::from_be_bytes(
            bytes[0..4]
                .try_into()
                .map_err(|_| CheckpointProofCodecError::InvalidLength)?,
        );
        let epoch = u64::from_be_bytes(
            bytes[4..12]
                .try_into()
                .map_err(|_| CheckpointProofCodecError::InvalidLength)?,
        );
        let next_currency_address = u64::from_be_bytes(
            bytes[12..20]
                .try_into()
                .map_err(|_| CheckpointProofCodecError::InvalidLength)?,
        );
        let current_supply = u64::from_be_bytes(
            bytes[20..28]
                .try_into()
                .map_err(|_| CheckpointProofCodecError::InvalidLength)?,
        );
        let reserve_count = u64::from_be_bytes(
            bytes[28..36]
                .try_into()
                .map_err(|_| CheckpointProofCodecError::InvalidLength)?,
        );
        let occupied_count = u64::from_be_bytes(
            bytes[36..44]
                .try_into()
                .map_err(|_| CheckpointProofCodecError::InvalidLength)?,
        );
        let state_digest = bytes[44..76]
            .try_into()
            .map_err(|_| CheckpointProofCodecError::InvalidLength)?;
        let validator_set_version = u64::from_be_bytes(
            bytes[76..84]
                .try_into()
                .map_err(|_| CheckpointProofCodecError::InvalidLength)?,
        );
        let vote_count = usize::try_from(u64::from_be_bytes(
            bytes[84..92]
                .try_into()
                .map_err(|_| CheckpointProofCodecError::InvalidLength)?,
        ))
        .map_err(|_| CheckpointProofCodecError::LengthOverflow)?;

        let expected_len = vote_count
            .checked_mul(CHECKPOINT_VOTE_ENCODED_SIZE)
            .and_then(|len| CHECKPOINT_PROOF_FIXED_SIZE.checked_add(len))
            .ok_or(CheckpointProofCodecError::LengthOverflow)?;
        if bytes.len() != expected_len {
            return Err(CheckpointProofCodecError::InvalidLength);
        }

        let mut votes = Vec::with_capacity(vote_count);
        let mut offset = CHECKPOINT_PROOF_FIXED_SIZE;
        for _ in 0..vote_count {
            let validator_id = crate::ValidatorId::new(u64::from_be_bytes(
                bytes[offset..offset + 8]
                    .try_into()
                    .map_err(|_| CheckpointProofCodecError::InvalidLength)?,
            ));
            offset += 8;
            let signature = bytes[offset..offset + 64]
                .try_into()
                .map_err(|_| CheckpointProofCodecError::InvalidLength)?;
            offset += 64;
            votes.push(ValidatorVote::from_untrusted_parts(validator_id, signature));
        }

        Ok(Self::new(
            PublicCurrencyCheckpoint::new(
                protocol_version,
                epoch,
                PublicCurrencySummary {
                    next_currency_address,
                    current_supply,
                    reserve_count,
                    occupied_count,
                    state_digest,
                },
            ),
            validator_set_version,
            votes,
        ))
    }

    pub fn verify(
        self,
        view: &PublicCurrencyView,
        validator_set: &ValidatorSet,
    ) -> Result<CertifiedPublicCurrencyCheckpoint, PublicCheckpointError> {
        if self.checkpoint.summary() != &view.summary {
            return Err(PublicCheckpointError::SummaryMismatch);
        }

        self.verify_checkpoint(validator_set)
    }

    pub fn verify_checkpoint(
        self,
        validator_set: &ValidatorSet,
    ) -> Result<CertifiedPublicCurrencyCheckpoint, PublicCheckpointError> {
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

    pub fn to_unverified_proof(&self) -> PublicCurrencyCheckpointProof {
        PublicCurrencyCheckpointProof::new(
            self.checkpoint.clone(),
            self.certificate.statement().validator_set_version(),
            self.certificate.votes().to_vec(),
        )
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
