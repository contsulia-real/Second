use crate::{
    AccountAddress, BftPhase, BftValue, CurrencyAddress, OperationClaimId, PaymentAddress,
    ValidatorId,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TaskEncodingError {
    LengthOverflow,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SignatureParseError {
    WrongEncodedLength,
    InvalidBase64Url,
    WrongDecodedLength,
    NonCanonical,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TaskValidationError {
    TransferAmountZero,
    IssueAmountZero,
    EmptyDestroy,
    EmptyLeakRepair,
    DuplicateCurrency(CurrencyAddress),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthorizationError {
    EmptyAuthorizerSet,
    DuplicateAuthorizer,
    UnsupportedProtocolVersion { expected: u32, actual: u32 },
    UntrustedAuthorizer,
    InvalidPublicKey,
    InvalidSignature,
    InvalidPayload(TaskValidationError),
    Encoding(TaskEncodingError),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecutionError {
    TaskIdAlreadyBound,
    TaskExpired,
    OperationIndexOverflow,
    AccountNotFound(AccountAddress),
    PaymentAddressAlreadyExists(PaymentAddress),
    PaymentAddressUnavailable(PaymentAddress),
    InvalidPaymentAddressTransition(PaymentAddress),
    InFlightTransferMismatch(OperationClaimId),
    TransferNotEstablished(OperationClaimId),
    InsufficientBalance {
        account: AccountAddress,
        required: u64,
        available: u64,
    },
    CurrencyNotFound(CurrencyAddress),
    CurrencyStillOccupied(CurrencyAddress),
    CurrencyNotCirculation(CurrencyAddress),
    CurrencyNotOwned(CurrencyAddress),
    DuplicateCurrency(CurrencyAddress),
    ReserveUnavailable {
        required: u64,
        available: u64,
    },
    CurrencySequenceSpaceExhausted,
    CurrencyAllocationFailed {
        requested: u64,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValidatorSetError {
    EmptySet,
    DuplicateValidator,
    DuplicateValidatorKey,
    InvalidIdentityKey(ValidatorId),
    InvalidConsensusKey(ValidatorId),
    InvalidRecoveryKey(ValidatorId),
    ReusedCredentialKey(ValidatorId),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PersistenceError {
    Io(std::io::ErrorKind),
    SnapshotTooLarge,
    UnsupportedSnapshotVersion(u32),
    ChecksumMismatch,
    InvalidSnapshot,
    NoValidSnapshot,
    MissingSnapshot,
    AlreadyInitialized,
    StoreLockPoisoned,
    StaleState,
    StalePreparedTasks,
    ValidatorRegistryMismatch,
    ValidatorTransition(ValidatorTransitionError),
    GenerationOverflow,
    ConflictingSnapshotGeneration(u64),
    CheckpointDoesNotMatchState,
    RecoveryCheckpointDoesNotMatchState,
    RecoveryValidatorSetMismatch,
    RecoveryCheckpointFinality(FinalityError),
    StaleRecoveryCheckpointSerial {
        validator_set_version: u64,
        minimum: u64,
        actual: u64,
    },
    RecoveryCheckpointFloorConflict {
        validator_set_version: u64,
        serial: u64,
        locked_digest: [u8; 32],
        attempted_digest: [u8; 32],
    },
    UnexpectedRecoveryCheckpointSerial {
        validator_set_version: u64,
        expected: u64,
        actual: u64,
    },
    RecoveryCheckpointAwaitingFinality {
        validator_set_version: u64,
        serial: u64,
    },
    RecoveryCheckpointSerialOverflow {
        validator_set_version: u64,
    },
    Bft(BftError),
    BftRoundMismatch {
        current: u64,
        attempted: u64,
    },
    BftRoundMustAdvance {
        current: u64,
        attempted: u64,
    },
    BftRoundOverflow {
        current: u64,
    },
    BftVoteConflict {
        phase: BftPhase,
        round: u64,
        locked: BftValue,
        attempted: BftValue,
    },
    BftUnlockProofRequired {
        locked_round: u64,
        attempted_digest: [u8; 32],
    },
    BftInvalidUnlockProof,
    BftPrevoteCertificateRequired,
    BftFinalityNotReady,
    ValidatorSafetyStateUnavailable,
    SigningFenceViolation {
        minimum_validator_set_version: u64,
        actual_validator_set_version: u64,
    },
    InvalidValidatorSafetyRecovery,
    RecoveringValidatorVotedSafetyFenceTransition(ValidatorId),
    CheckpointValidatorSetMismatch {
        expected: u64,
        actual: u64,
    },
    CheckpointFinality(FinalityError),
    StaleCheckpointEpoch {
        minimum: u64,
        actual: u64,
    },
}

impl PersistenceError {
    pub(crate) fn from_io(error: std::io::Error) -> Self {
        Self::Io(error.kind())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BftError {
    WrongProtocolVersion {
        expected: u32,
        actual: u32,
    },
    WrongValidatorSetVersion {
        expected: u64,
        actual: u64,
    },
    ScopeValidatorSetVersionMismatch {
        expected: u64,
        actual: u64,
    },
    UnknownValidator(ValidatorId),
    DuplicateVote(ValidatorId),
    InvalidValidatorKey(ValidatorId),
    InvalidSignature(ValidatorId),
    WrongProposer {
        expected: ValidatorId,
        actual: ValidatorId,
    },
    InsufficientVotes {
        required: usize,
        actual: usize,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FinalityError {
    WrongProtocolVersion { expected: u32, actual: u32 },
    WrongValidatorSetVersion { expected: u64, actual: u64 },
    UnknownValidator(ValidatorId),
    DuplicateVote(ValidatorId),
    InvalidValidatorKey(ValidatorId),
    InvalidSignature(ValidatorId),
    InsufficientVotes { required: usize, actual: usize },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValidatorAdmissionError {
    UnsupportedProtocolVersion { expected: u32, actual: u32 },
    InvalidIdentityProof(ValidatorId),
    InvalidConsensusProof(ValidatorId),
    InvalidRecoveryProof(ValidatorId),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValidatorRotationError {
    UnsupportedProtocolVersion {
        expected: u32,
        actual: u32,
    },
    ValidatorIdMismatch {
        expected: ValidatorId,
        actual: ValidatorId,
    },
    InvalidNewConsensusKey(ValidatorId),
    InvalidAuthorization(ValidatorId),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValidatorRegistryError {
    CurrentSetVersionMismatch { expected: u64, actual: u64 },
    CurrentSetMembershipMismatch,
    CurrentSetCredentialMismatch(ValidatorId),
    WrongNextValidatorSetVersion { expected: u64, actual: u64 },
    ValidatorSetVersionOverflow,
    ValidatorIdAlreadyUsed(ValidatorId),
    ValidatorKeyAlreadyUsed(ValidatorId),
    IdentityKeyChanged(ValidatorId),
    RecoveryKeyChanged(ValidatorId),
    InvalidConsensusKeyHistory(ValidatorId),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValidatorTransitionError {
    UnsupportedProtocolVersion {
        expected: u32,
        actual: u32,
    },
    WrongCurrentValidatorSetVersion {
        expected: u64,
        actual: u64,
    },
    WrongNextValidatorSetVersion {
        expected: u64,
        actual: u64,
    },
    ValidatorSetVersionOverflow,
    IdentityKeyChanged(ValidatorId),
    RecoveryKeyChanged(ValidatorId),
    Registry(ValidatorRegistryError),
    MissingConsensusKeyRotation(ValidatorId),
    UnexpectedConsensusKeyRotation(ValidatorId),
    DuplicateConsensusKeyRotation(ValidatorId),
    ConsensusKeyRotationCredentialMismatch(ValidatorId),
    ConsensusKeyRotationValidatorSetMismatch {
        validator_id: ValidatorId,
        expected: u64,
        actual: u64,
    },
    Rotation(ValidatorRotationError),
    MissingAdmission(ValidatorId),
    UnexpectedAdmission(ValidatorId),
    DuplicateAdmission(ValidatorId),
    AdmissionCredentialMismatch(ValidatorId),
    Finality(FinalityError),
}
