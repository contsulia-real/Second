use std::collections::BTreeSet;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

use crate::{BftError, CURRENT_PROTOCOL_VERSION, TaskId, ValidatorId, ValidatorSet};

const BFT_DOMAIN: &[u8] = b"SECOND_BFT_V1\0";

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum ConsensusScope {
    CurrencyAllocation {
        validator_set_version: u64,
        start: u64,
    },
    PreparedTask(TaskId),
    PublicCheckpoint {
        validator_set_version: u64,
        epoch: u64,
    },
    StateRecoveryCheckpoint {
        validator_set_version: u64,
        serial: u64,
    },
}

impl ConsensusScope {
    pub(crate) fn matches_validator_set_version(&self, version: u64) -> bool {
        match self {
            Self::PreparedTask(_) => true,
            Self::PublicCheckpoint {
                validator_set_version,
                ..
            }
            | Self::StateRecoveryCheckpoint {
                validator_set_version,
                ..
            }
            | Self::CurrencyAllocation {
                validator_set_version,
                ..
            } => *validator_set_version == version,
        }
    }

    pub(crate) fn explicit_validator_set_version(&self) -> Option<u64> {
        match self {
            Self::PreparedTask(_) => None,
            Self::PublicCheckpoint {
                validator_set_version,
                ..
            }
            | Self::StateRecoveryCheckpoint {
                validator_set_version,
                ..
            }
            | Self::CurrencyAllocation {
                validator_set_version,
                ..
            } => Some(*validator_set_version),
        }
    }

    pub(crate) fn encode_canonical(&self, out: &mut Vec<u8>) {
        match self {
            Self::CurrencyAllocation {
                validator_set_version,
                start,
            } => {
                out.push(5);
                out.extend_from_slice(&validator_set_version.to_be_bytes());
                out.extend_from_slice(&start.to_be_bytes());
            }
            Self::PreparedTask(task_id) => {
                out.push(1);
                out.push(task_id.len() as u8);
                out.extend_from_slice(task_id.as_bytes());
            }
            Self::PublicCheckpoint {
                validator_set_version,
                epoch,
            } => {
                out.push(2);
                out.extend_from_slice(&validator_set_version.to_be_bytes());
                out.extend_from_slice(&epoch.to_be_bytes());
            }
            Self::StateRecoveryCheckpoint {
                validator_set_version,
                serial,
            } => {
                out.push(4);
                out.extend_from_slice(&validator_set_version.to_be_bytes());
                out.extend_from_slice(&serial.to_be_bytes());
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum BftPhase {
    Prevote,
    Precommit,
}

impl BftPhase {
    const fn tag(self) -> u8 {
        match self {
            Self::Prevote => 1,
            Self::Precommit => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum BftValue {
    Nil,
    Digest([u8; 32]),
}

impl BftValue {
    fn encode_canonical(self, out: &mut Vec<u8>) {
        match self {
            Self::Nil => out.push(0),
            Self::Digest(digest) => {
                out.push(1);
                out.extend_from_slice(&digest);
            }
        }
    }

    pub const fn digest(self) -> Option<[u8; 32]> {
        match self {
            Self::Nil => None,
            Self::Digest(digest) => Some(digest),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BftStatement {
    protocol_version: u32,
    validator_set_version: u64,
    scope: ConsensusScope,
    round: u64,
    phase: BftPhase,
    value: BftValue,
}

impl BftStatement {
    pub fn new(
        validator_set_version: u64,
        scope: ConsensusScope,
        round: u64,
        phase: BftPhase,
        value: BftValue,
    ) -> Self {
        Self {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            validator_set_version,
            scope,
            round,
            phase,
            value,
        }
    }

    pub fn from_untrusted_parts(
        protocol_version: u32,
        validator_set_version: u64,
        scope: ConsensusScope,
        round: u64,
        phase: BftPhase,
        value: BftValue,
    ) -> Self {
        Self {
            protocol_version,
            validator_set_version,
            scope,
            round,
            phase,
            value,
        }
    }

    pub const fn protocol_version(&self) -> u32 {
        self.protocol_version
    }

    pub const fn validator_set_version(&self) -> u64 {
        self.validator_set_version
    }

    pub fn scope(&self) -> &ConsensusScope {
        &self.scope
    }

    pub const fn round(&self) -> u64 {
        self.round
    }

    pub const fn phase(&self) -> BftPhase {
        self.phase
    }

    pub const fn value(&self) -> BftValue {
        self.value
    }

    pub fn canonical_signing_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(BFT_DOMAIN.len() + 4 + 8 + 32 + 8 + 2);
        out.extend_from_slice(BFT_DOMAIN);
        out.extend_from_slice(&self.protocol_version.to_be_bytes());
        out.extend_from_slice(&self.validator_set_version.to_be_bytes());
        self.scope.encode_canonical(&mut out);
        out.extend_from_slice(&self.round.to_be_bytes());
        out.push(self.phase.tag());
        self.value.encode_canonical(&mut out);
        out
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BftVote {
    validator_id: ValidatorId,
    signature: [u8; 64],
}

impl BftVote {
    pub(crate) fn sign_unchecked(
        statement: &BftStatement,
        validator_id: ValidatorId,
        signing_key: &SigningKey,
    ) -> Self {
        Self {
            validator_id,
            signature: signing_key
                .sign(&statement.canonical_signing_bytes())
                .to_bytes(),
        }
    }

    pub const fn from_untrusted_parts(validator_id: ValidatorId, signature: [u8; 64]) -> Self {
        Self {
            validator_id,
            signature,
        }
    }

    pub const fn validator_id(&self) -> ValidatorId {
        self.validator_id
    }

    pub const fn signature_bytes(&self) -> [u8; 64] {
        self.signature
    }

    pub fn verify(
        &self,
        statement: &BftStatement,
        validator_set: &ValidatorSet,
    ) -> Result<(), BftError> {
        validate_statement(statement, validator_set)?;
        let credential = validator_set
            .validator(self.validator_id)
            .ok_or(BftError::UnknownValidator(self.validator_id))?;
        let key = VerifyingKey::from_bytes(&credential.consensus_public_key())
            .map_err(|_| BftError::InvalidValidatorKey(self.validator_id))?;
        key.verify_strict(
            &statement.canonical_signing_bytes(),
            &Signature::from_bytes(&self.signature),
        )
        .map_err(|_| BftError::InvalidSignature(self.validator_id))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BftQuorumCertificate {
    statement: BftStatement,
    votes: Vec<BftVote>,
}

impl BftQuorumCertificate {
    pub fn from_untrusted_parts(statement: BftStatement, votes: Vec<BftVote>) -> Self {
        Self { statement, votes }
    }

    pub fn new(
        statement: BftStatement,
        votes: Vec<BftVote>,
        validator_set: &ValidatorSet,
    ) -> Result<Self, BftError> {
        let certificate = Self { statement, votes };
        certificate.verify(validator_set)?;
        Ok(certificate)
    }

    pub fn statement(&self) -> &BftStatement {
        &self.statement
    }

    pub fn votes(&self) -> &[BftVote] {
        &self.votes
    }

    pub fn verify(&self, validator_set: &ValidatorSet) -> Result<(), BftError> {
        validate_statement(&self.statement, validator_set)?;
        let mut seen = BTreeSet::new();
        for vote in &self.votes {
            if !seen.insert(vote.validator_id()) {
                return Err(BftError::DuplicateVote(vote.validator_id()));
            }
            vote.verify(&self.statement, validator_set)?;
        }

        if seen.len() < validator_set.quorum_threshold() {
            return Err(BftError::InsufficientVotes {
                required: validator_set.quorum_threshold(),
                actual: seen.len(),
            });
        }
        Ok(())
    }
}

fn validate_statement(
    statement: &BftStatement,
    validator_set: &ValidatorSet,
) -> Result<(), BftError> {
    if statement.protocol_version() != CURRENT_PROTOCOL_VERSION {
        return Err(BftError::WrongProtocolVersion {
            expected: CURRENT_PROTOCOL_VERSION,
            actual: statement.protocol_version(),
        });
    }
    if statement.validator_set_version() != validator_set.version() {
        return Err(BftError::WrongValidatorSetVersion {
            expected: validator_set.version(),
            actual: statement.validator_set_version(),
        });
    }
    if let Some(actual) = statement.scope().explicit_validator_set_version()
        && actual != statement.validator_set_version()
    {
        return Err(BftError::ScopeValidatorSetVersionMismatch {
            expected: statement.validator_set_version(),
            actual,
        });
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BftLocalState {
    validator_set_version: u64,
    round: u64,
    locked_round: Option<u64>,
    locked_digest: Option<[u8; 32]>,
    valid_prevote_qc: Option<BftQuorumCertificate>,
    prevote: Option<BftValue>,
    precommit: Option<BftValue>,
    finality_ready_round: Option<u64>,
    finality_ready_digest: Option<[u8; 32]>,
}

impl BftLocalState {
    pub(crate) fn new(validator_set_version: u64) -> Self {
        Self {
            validator_set_version,
            round: 0,
            locked_round: None,
            locked_digest: None,
            valid_prevote_qc: None,
            prevote: None,
            precommit: None,
            finality_ready_round: None,
            finality_ready_digest: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_persisted(
        validator_set_version: u64,
        round: u64,
        locked_round: Option<u64>,
        locked_digest: Option<[u8; 32]>,
        valid_prevote_qc: Option<BftQuorumCertificate>,
        prevote: Option<BftValue>,
        precommit: Option<BftValue>,
        finality_ready_round: Option<u64>,
        finality_ready_digest: Option<[u8; 32]>,
    ) -> Self {
        Self {
            validator_set_version,
            round,
            locked_round,
            locked_digest,
            valid_prevote_qc,
            prevote,
            precommit,
            finality_ready_round,
            finality_ready_digest,
        }
    }

    pub const fn validator_set_version(&self) -> u64 {
        self.validator_set_version
    }

    pub const fn round(&self) -> u64 {
        self.round
    }

    pub const fn locked_round(&self) -> Option<u64> {
        self.locked_round
    }

    pub const fn locked_digest(&self) -> Option<[u8; 32]> {
        self.locked_digest
    }

    pub fn valid_prevote_qc(&self) -> Option<&BftQuorumCertificate> {
        self.valid_prevote_qc.as_ref()
    }

    pub(crate) fn remember_prevote_qc(&mut self, certificate: &BftQuorumCertificate) -> bool {
        if certificate.statement().phase() != BftPhase::Prevote
            || !matches!(certificate.statement().value(), BftValue::Digest(_))
        {
            return false;
        }
        if self
            .valid_prevote_qc
            .as_ref()
            .is_some_and(|previous| previous.statement().round() >= certificate.statement().round())
        {
            return false;
        }
        self.valid_prevote_qc = Some(certificate.clone());
        true
    }

    pub(crate) const fn prevote(&self) -> Option<BftValue> {
        self.prevote
    }

    pub(crate) const fn precommit(&self) -> Option<BftValue> {
        self.precommit
    }

    pub(crate) const fn finality_ready_round(&self) -> Option<u64> {
        self.finality_ready_round
    }

    pub(crate) const fn finality_ready_digest(&self) -> Option<[u8; 32]> {
        self.finality_ready_digest
    }

    pub(crate) fn set_round(&mut self, round: u64) {
        self.round = round;
        self.prevote = None;
        self.precommit = None;
    }

    pub(crate) fn set_prevote(&mut self, value: BftValue) {
        self.prevote = Some(value);
    }

    pub(crate) fn set_precommit(&mut self, value: BftValue) {
        self.precommit = Some(value);
        if let BftValue::Digest(digest) = value {
            self.locked_round = Some(self.round);
            self.locked_digest = Some(digest);
        }
    }

    pub(crate) fn mark_finality_ready(&mut self, round: u64, digest: [u8; 32]) {
        self.finality_ready_round = Some(round);
        self.finality_ready_digest = Some(digest);
    }
}
