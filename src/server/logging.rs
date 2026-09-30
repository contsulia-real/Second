use std::env;
use std::fmt;
use std::io;

use tracing_subscriber::EnvFilter;
use tracing_subscriber::util::SubscriberInitExt;

const DEFAULT_LOG_FILTER: &str = "info";

#[derive(Debug)]
pub enum TracingInitError {
    InvalidFilter(String),
    Install(String),
}

impl fmt::Display for TracingInitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFilter(error) => write!(f, "SECOND_LOG_FILTER is invalid: {error}"),
            Self::Install(error) => {
                write!(f, "failed to install global tracing subscriber: {error}")
            }
        }
    }
}

impl std::error::Error for TracingInitError {}

/// Installs the process-global production subscriber before any listener or
/// database pool is created. Production logs are JSON on stderr only.
pub fn init_tracing_from_env() -> Result<(), TracingInitError> {
    let filter = log_filter_from_lookup(|name| env::var(name).ok())?;

    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .with_writer(io::stderr)
        .finish()
        .try_init()
        .map_err(|error| TracingInitError::Install(error.to_string()))
}

fn log_filter_from_lookup(
    mut lookup: impl FnMut(&str) -> Option<String>,
) -> Result<EnvFilter, TracingInitError> {
    let value = lookup("SECOND_LOG_FILTER").unwrap_or_else(|| DEFAULT_LOG_FILTER.to_owned());
    EnvFilter::try_new(value).map_err(|error| TracingInitError::InvalidFilter(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_log_filter_uses_the_only_protocol_default() {
        let filter = log_filter_from_lookup(|_| None).unwrap();
        assert_eq!(filter.to_string(), DEFAULT_LOG_FILTER);
    }

    #[test]
    fn explicit_invalid_log_filter_is_a_startup_error() {
        assert!(matches!(
            log_filter_from_lookup(|_| Some("[not a directive".to_owned())),
            Err(TracingInitError::InvalidFilter(_))
        ));
    }
}
