use crate::{AccountAddress, CurrencyAddress};

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
}
