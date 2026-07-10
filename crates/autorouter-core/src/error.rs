//! Core error types used across the AutoRouter workspace.

use thiserror::Error;

pub type CoreResult<T> = std::result::Result<T, CoreError>;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("validation error: {0}")]
    Validation(String),

    #[error("unsupported capability: {0}")]
    UnsupportedCapability(String),

    #[error("stream cancelled: {0}")]
    StreamCancelled(String),

    #[error("limit exceeded: {0}")]
    LimitExceeded(String),

    #[error("internal error: {0}")]
    Internal(String),
}

impl CoreError {
    pub fn is_client_visible(&self) -> bool {
        matches!(
            self,
            CoreError::Validation(_)
                | CoreError::UnsupportedCapability(_)
                | CoreError::LimitExceeded(_)
        )
    }
}
