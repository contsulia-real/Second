use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddressParseError {
    WrongPrefix,
    WrongEncodedLength,
    InvalidBase64Url,
    WrongDecodedLength,
    NonCanonical,
}

macro_rules! opaque_address_type {
    ($name:ident, $prefix:literal) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
        pub struct $name([u8; 32]);

        impl $name {
            pub const fn from_bytes(bytes: [u8; 32]) -> Self {
                Self(bytes)
            }

            pub const fn bytes(self) -> [u8; 32] {
                self.0
            }

            pub fn parse(value: &str) -> Result<Self, AddressParseError> {
                let encoded = value
                    .strip_prefix($prefix)
                    .ok_or(AddressParseError::WrongPrefix)?;

                if encoded.len() != 43 {
                    return Err(AddressParseError::WrongEncodedLength);
                }

                let decoded = URL_SAFE_NO_PAD
                    .decode(encoded)
                    .map_err(|_| AddressParseError::InvalidBase64Url)?;
                let bytes: [u8; 32] = decoded
                    .try_into()
                    .map_err(|_| AddressParseError::WrongDecodedLength)?;

                if URL_SAFE_NO_PAD.encode(bytes) != encoded {
                    return Err(AddressParseError::NonCanonical);
                }

                Ok(Self(bytes))
            }

            pub fn canonical_string(self) -> String {
                format!("{}{}", $prefix, URL_SAFE_NO_PAD.encode(self.0))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str($prefix)?;
                formatter.write_str(&URL_SAFE_NO_PAD.encode(self.0))
            }
        }

        impl FromStr for $name {
            type Err = AddressParseError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::parse(value)
            }
        }
    };
}

opaque_address_type!(AccountAddress, "acct_");
opaque_address_type!(PaymentAddress, "pay_");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskIdParseError {
    Empty,
    TooLong,
    InvalidCharacter,
}

#[derive(Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct TaskId(Arc<str>);

impl TaskId {
    pub fn parse(value: &str) -> Result<Self, TaskIdParseError> {
        validate_task_id_bytes(value.as_bytes())?;
        Ok(Self(Arc::from(value)))
    }

    pub fn from_ascii_bytes(value: &[u8]) -> Result<Self, TaskIdParseError> {
        validate_task_id_bytes(value)?;
        let value = std::str::from_utf8(value).map_err(|_| TaskIdParseError::InvalidCharacter)?;
        Ok(Self(Arc::from(value)))
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

fn validate_task_id_bytes(value: &[u8]) -> Result<(), TaskIdParseError> {
    if value.is_empty() {
        return Err(TaskIdParseError::Empty);
    }
    if value.len() > 128 {
        return Err(TaskIdParseError::TooLong);
    }
    if !value
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(TaskIdParseError::InvalidCharacter);
    }
    Ok(())
}

impl fmt::Display for TaskId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl fmt::Debug for TaskId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("TaskId").field(&self.0).finish()
    }
}

impl FromStr for TaskId {
    type Err = TaskIdParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

const CURRENCY_BASE62_ALPHABET: &[u8; 62] =
    b"0123456789aAbBcCdDeEfFgGhHiIjJkKlLmMnNoOpPqQrRsStTuUvVwWxXyYzZ";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CurrencyAddressParseError {
    WrongPrefix,
    EmptyPayload,
    InvalidCharacter,
    Overflow,
    NonCanonical,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct CurrencyAddress(u64);

impl CurrencyAddress {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u64 {
        self.0
    }

    pub fn parse(value: &str) -> Result<Self, CurrencyAddressParseError> {
        let payload = value
            .strip_prefix("0lxii")
            .ok_or(CurrencyAddressParseError::WrongPrefix)?;
        if payload.is_empty() {
            return Err(CurrencyAddressParseError::EmptyPayload);
        }

        let mut sequence = 0_u64;
        for byte in payload.bytes() {
            let digit = CURRENCY_BASE62_ALPHABET
                .iter()
                .position(|candidate| *candidate == byte)
                .ok_or(CurrencyAddressParseError::InvalidCharacter)? as u64;
            sequence = sequence
                .checked_mul(62)
                .and_then(|current| current.checked_add(digit))
                .ok_or(CurrencyAddressParseError::Overflow)?;
        }

        let address = Self(sequence);
        if address.canonical_string() != value {
            return Err(CurrencyAddressParseError::NonCanonical);
        }
        Ok(address)
    }

    pub fn canonical_string(self) -> String {
        let mut text = String::from("0lxii");
        if self.0 == 0 {
            text.push('0');
            return text;
        }

        let mut value = self.0;
        let mut reversed = [0_u8; 11];
        let mut len = 0_usize;
        while value != 0 {
            reversed[len] = CURRENCY_BASE62_ALPHABET[(value % 62) as usize];
            len += 1;
            value /= 62;
        }
        for byte in reversed[..len].iter().rev() {
            text.push(char::from(*byte));
        }
        text
    }
}

impl fmt::Display for CurrencyAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.canonical_string())
    }
}

impl FromStr for CurrencyAddress {
    type Err = CurrencyAddressParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl From<u64> for CurrencyAddress {
    fn from(value: u64) -> Self {
        Self::new(value)
    }
}

macro_rules! numeric_id_type {
    ($name:ident, $inner:ty) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
        pub struct $name($inner);

        impl $name {
            pub const fn new(value: $inner) -> Self {
                Self(value)
            }

            pub const fn value(self) -> $inner {
                self.0
            }
        }
    };
}

numeric_id_type!(ValidatorId, u64);
