use crate::{AccountAddress, CurrencyAddress, ValidatorId};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TaskEncodingError {
    LengthOverflow,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthorizationError {
    EmptyAuthorizerSet,
    DuplicateAuthorizer,
    UnsupportedProtocolVersion { expected: u32, actual: u32 },
    UntrustedAuthorizer,
    InvalidPublicKey,
    InvalidSignature,
    Encoding(TaskEncodingError),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecutionError {
    TaskIdAlreadyBound,
    TaskExpired,
    AccountNotFound(AccountAddress),
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
    IdentitySpaceExhausted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValidatorSetError {
    EmptySet,
    DuplicateValidator,
    DuplicateConsensusKey,
    InvalidConsensusKey(ValidatorId),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PersistenceError {
    Io(std::io::ErrorKind),
    SnapshotTooLarge,
    UnsupportedSnapshotVersion(u32),
    ChecksumMismatch,
    InvalidSnapshot,
    NoValidSnapshot,
    GenerationOverflow,
}

impl PersistenceError {
    pub(crate) fn from_io(error: std::io::Error) -> Self {
        Self::Io(error.kind())
    }
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
