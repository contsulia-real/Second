mod codec;
mod session;

use std::fmt;
use std::io;

use crate::{CurrencyAddress, PublicCurrencyState};

pub use codec::{read_network_message, write_network_message};
pub use session::{
    client_ping, client_public_currency_page, serve_ping_session, serve_public_currency_connection,
    serve_public_currency_session,
};

pub const MAX_NETWORK_FRAME_SIZE: usize = 64 * 1024;
pub const MAX_PUBLIC_CURRENCY_PAGE: u16 = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct NodeId([u8; 32]);

impl NodeId {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn from_u64(value: u64) -> Self {
        let mut bytes = [0_u8; 32];
        bytes[24..].copy_from_slice(&value.to_be_bytes());
        Self(bytes)
    }

    pub const fn to_bytes(self) -> [u8; 32] {
        self.0
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicCurrencyPage {
    pub states: Vec<PublicCurrencyState>,
    pub next_start: Option<CurrencyAddress>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemotePublicCurrencyPage {
    pub remote_node_id: NodeId,
    pub states: Vec<PublicCurrencyState>,
    pub next_start: Option<CurrencyAddress>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NetworkMessage {
    Hello {
        node_id: NodeId,
    },
    Ping {
        nonce: u64,
    },
    Pong {
        nonce: u64,
    },
    GetPublicCurrencies {
        start: CurrencyAddress,
        span: u16,
    },
    PublicCurrencies {
        states: Vec<PublicCurrencyState>,
        next_start: Option<CurrencyAddress>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NetworkError {
    Io(io::ErrorKind),
    InvalidMagic,
    UnsupportedProtocolVersion {
        expected: u32,
        actual: u32,
    },
    FrameTooLarge {
        announced: usize,
        maximum: usize,
    },
    EmptyPayload,
    UnknownMessageType(u8),
    InvalidMessageLength {
        message_type: u8,
        expected: usize,
        actual: usize,
    },
    InvalidCurrencyRole(u8),
    InvalidBoolean(u8),
    InvalidCursorFlag(u8),
    InvalidPublicCurrencySpan {
        requested: u16,
        maximum: u16,
    },
    TooManyPublicCurrencyStates {
        announced: usize,
        maximum: usize,
    },
    UnexpectedMessage,
    NonceMismatch {
        expected: u64,
        actual: u64,
    },
}

impl From<io::Error> for NetworkError {
    fn from(value: io::Error) -> Self {
        Self::Io(value.kind())
    }
}

pub(crate) fn validate_public_currency_span(span: u16) -> Result<(), NetworkError> {
    if span == 0 || span > MAX_PUBLIC_CURRENCY_PAGE {
        Err(NetworkError::InvalidPublicCurrencySpan {
            requested: span,
            maximum: MAX_PUBLIC_CURRENCY_PAGE,
        })
    } else {
        Ok(())
    }
}
