//! Routing errors.

use thiserror::Error;

pub type RoutingResult<T> = std::result::Result<T, RoutingError>;

#[derive(Debug, Error)]
pub enum RoutingError {
    #[error("no route available: {0}")]
    NoRoute(String),

    #[error("provider disabled: {0}")]
    ProviderDisabled(String),

    #[error("capability mismatch: {0}")]
    CapabilityMismatch(String),

    #[error("upstream error: {0}")]
    Upstream(String),
}
