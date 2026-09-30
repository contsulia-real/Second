use std::env;
use std::fmt;
use std::net::SocketAddr;
use std::str::FromStr;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sqlx::postgres::PgConnectOptions;

/// Fully validated production runtime configuration.
///
/// Every production knob is explicit except SECOND_LOG_FILTER, which belongs to
/// tracing initialization and has the protocol-defined default of "info".
#[derive(Clone, Debug)]
pub struct ServerConfig {
    database: PgConnectOptions,
    issuer_public_key: [u8; 32],
    selection_secret: Vec<u8>,
    bind_addr: SocketAddr,
    execution_pool_size: u32,
    prerequisite_pool_size: u32,
    reaper_interval: Duration,
    reaper_batch_size: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServerConfigError {
    Missing(&'static str),
    InvalidDatabaseUrl,
    InvalidIssuerPublicKey,
    InvalidSelectionSecret,
    InvalidBindAddress,
    InvalidPositiveInteger(&'static str),
}

impl fmt::Display for ServerConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(name) => write!(f, "missing required environment variable {name}"),
            Self::InvalidDatabaseUrl => f.write_str("SECOND_DATABASE_URL is invalid"),
            Self::InvalidIssuerPublicKey => {
                f.write_str("SECOND_ISSUER_PUBLIC_KEY must be canonical Base64URL without padding and decode to 32 bytes")
            }
            Self::InvalidSelectionSecret => {
                f.write_str("SECOND_SELECTION_SECRET must be canonical Base64URL without padding")
            }
            Self::InvalidBindAddress => f.write_str("SECOND_BIND_ADDR is invalid"),
            Self::InvalidPositiveInteger(name) => {
                write!(f, "{name} must be a non-zero integer in range")
            }
        }
    }
}

impl std::error::Error for ServerConfigError {}

impl ServerConfig {
    pub fn from_env() -> Result<Self, ServerConfigError> {
        Self::from_lookup(|name| env::var(name).ok())
    }

    fn from_lookup(
        mut lookup: impl FnMut(&str) -> Option<String>,
    ) -> Result<Self, ServerConfigError> {
        let database_url = required(&mut lookup, "SECOND_DATABASE_URL")?;
        let database = PgConnectOptions::from_str(&database_url)
            .map_err(|_| ServerConfigError::InvalidDatabaseUrl)?;

        let issuer_text = required(&mut lookup, "SECOND_ISSUER_PUBLIC_KEY")?;
        let issuer_bytes = decode_canonical_base64url(&issuer_text)
            .map_err(|_| ServerConfigError::InvalidIssuerPublicKey)?;
        let issuer_public_key: [u8; 32] = issuer_bytes
            .try_into()
            .map_err(|_| ServerConfigError::InvalidIssuerPublicKey)?;

        let selection_text = required(&mut lookup, "SECOND_SELECTION_SECRET")?;
        let selection_secret = decode_canonical_base64url(&selection_text)
            .map_err(|_| ServerConfigError::InvalidSelectionSecret)?;

        let bind_addr = required(&mut lookup, "SECOND_BIND_ADDR")?
            .parse()
            .map_err(|_| ServerConfigError::InvalidBindAddress)?;

        let execution_pool_size = parse_nonzero_u32(&mut lookup, "SECOND_EXECUTION_POOL_SIZE")?;
        let prerequisite_pool_size =
            parse_nonzero_u32(&mut lookup, "SECOND_PREREQUISITE_POOL_SIZE")?;
        let reaper_interval_ms = parse_nonzero_u64(&mut lookup, "SECOND_REAPER_INTERVAL")?;
        let reaper_batch_size = parse_nonzero_u32(&mut lookup, "SECOND_REAPER_BATCH_SIZE")?;

        Ok(Self {
            database,
            issuer_public_key,
            selection_secret,
            bind_addr,
            execution_pool_size,
            prerequisite_pool_size,
            reaper_interval: Duration::from_millis(reaper_interval_ms),
            reaper_batch_size,
        })
    }

    pub fn database(&self) -> &PgConnectOptions {
        &self.database
    }

    pub const fn issuer_public_key(&self) -> [u8; 32] {
        self.issuer_public_key
    }

    pub fn selection_secret(&self) -> &[u8] {
        &self.selection_secret
    }

    pub const fn bind_addr(&self) -> SocketAddr {
        self.bind_addr
    }

    pub const fn execution_pool_size(&self) -> u32 {
        self.execution_pool_size
    }

    pub const fn prerequisite_pool_size(&self) -> u32 {
        self.prerequisite_pool_size
    }

    pub const fn reaper_interval(&self) -> Duration {
        self.reaper_interval
    }

    pub const fn reaper_batch_size(&self) -> u32 {
        self.reaper_batch_size
    }
}

fn required(
    lookup: &mut impl FnMut(&str) -> Option<String>,
    name: &'static str,
) -> Result<String, ServerConfigError> {
    lookup(name).ok_or(ServerConfigError::Missing(name))
}

fn parse_nonzero_u32(
    lookup: &mut impl FnMut(&str) -> Option<String>,
    name: &'static str,
) -> Result<u32, ServerConfigError> {
    let value = required(lookup, name)?;
    let value = value
        .parse::<u32>()
        .map_err(|_| ServerConfigError::InvalidPositiveInteger(name))?;
    if value == 0 {
        return Err(ServerConfigError::InvalidPositiveInteger(name));
    }
    Ok(value)
}

fn parse_nonzero_u64(
    lookup: &mut impl FnMut(&str) -> Option<String>,
    name: &'static str,
) -> Result<u64, ServerConfigError> {
    let value = required(lookup, name)?;
    let value = value
        .parse::<u64>()
        .map_err(|_| ServerConfigError::InvalidPositiveInteger(name))?;
    if value == 0 {
        return Err(ServerConfigError::InvalidPositiveInteger(name));
    }
    Ok(value)
}

fn decode_canonical_base64url(value: &str) -> Result<Vec<u8>, ()> {
    let decoded = URL_SAFE_NO_PAD.decode(value).map_err(|_| ())?;
    if URL_SAFE_NO_PAD.encode(&decoded) != value {
        return Err(());
    }
    Ok(decoded)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    use super::*;

    fn valid_env() -> BTreeMap<&'static str, String> {
        BTreeMap::from([
            (
                "SECOND_DATABASE_URL",
                "postgres://second:secret@localhost/second".to_owned(),
            ),
            (
                "SECOND_ISSUER_PUBLIC_KEY",
                URL_SAFE_NO_PAD.encode([7_u8; 32]),
            ),
            (
                "SECOND_SELECTION_SECRET",
                URL_SAFE_NO_PAD.encode(b"selection secret"),
            ),
            ("SECOND_BIND_ADDR", "127.0.0.1:8080".to_owned()),
            ("SECOND_EXECUTION_POOL_SIZE", "8".to_owned()),
            ("SECOND_PREREQUISITE_POOL_SIZE", "4".to_owned()),
            ("SECOND_REAPER_INTERVAL", "250".to_owned()),
            ("SECOND_REAPER_BATCH_SIZE", "100".to_owned()),
        ])
    }

    fn parse(values: &BTreeMap<&'static str, String>) -> Result<ServerConfig, ServerConfigError> {
        ServerConfig::from_lookup(|name| values.get(name).cloned())
    }

    #[test]
    fn production_config_requires_every_explicit_runtime_value() {
        let mut values = valid_env();
        values.remove("SECOND_DATABASE_URL");

        assert_eq!(
            parse(&values).unwrap_err(),
            ServerConfigError::Missing("SECOND_DATABASE_URL")
        );
    }

    #[test]
    fn issuer_key_and_selection_secret_require_canonical_base64url() {
        let mut issuer = valid_env();
        issuer.insert(
            "SECOND_ISSUER_PUBLIC_KEY",
            format!("{}=", URL_SAFE_NO_PAD.encode([7_u8; 32])),
        );
        assert_eq!(
            parse(&issuer).unwrap_err(),
            ServerConfigError::InvalidIssuerPublicKey
        );

        let mut secret = valid_env();
        secret.insert("SECOND_SELECTION_SECRET", "not+url/base64".to_owned());
        assert_eq!(
            parse(&secret).unwrap_err(),
            ServerConfigError::InvalidSelectionSecret
        );
    }

    #[test]
    fn pool_and_reaper_sizes_must_be_nonzero() {
        for name in [
            "SECOND_EXECUTION_POOL_SIZE",
            "SECOND_PREREQUISITE_POOL_SIZE",
            "SECOND_REAPER_INTERVAL",
            "SECOND_REAPER_BATCH_SIZE",
        ] {
            let mut values = valid_env();
            values.insert(name, "0".to_owned());
            assert_eq!(
                parse(&values).unwrap_err(),
                ServerConfigError::InvalidPositiveInteger(name)
            );
        }
    }

    #[test]
    fn valid_config_preserves_protocol_runtime_values() {
        let values = valid_env();
        let config = parse(&values).unwrap();

        assert_eq!(config.issuer_public_key(), [7_u8; 32]);
        assert_eq!(config.selection_secret(), b"selection secret");
        assert_eq!(config.bind_addr(), "127.0.0.1:8080".parse().unwrap());
        assert_eq!(config.execution_pool_size(), 8);
        assert_eq!(config.prerequisite_pool_size(), 4);
        assert_eq!(config.reaper_interval(), Duration::from_millis(250));
        assert_eq!(config.reaper_batch_size(), 100);
    }
}
