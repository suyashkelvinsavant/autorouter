//! Observability errors.

use thiserror::Error;

pub type ObservabilityResult<T> = std::result::Result<T, ObservabilityError>;

#[derive(Debug, Error)]
pub enum ObservabilityError {
    #[error("logging init error: {0}")]
    Logging(String),

    #[error("metrics error: {0}")]
    Metrics(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
