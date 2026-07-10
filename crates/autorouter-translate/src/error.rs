//! Translation errors.

use thiserror::Error;

use autorouter_core::CoreError;

pub type TranslateResult<T> = std::result::Result<T, TranslateError>;

#[derive(Debug, Error)]
pub enum TranslateError {
    #[error("unsupported source provider: {0}")]
    UnsupportedSource(String),

    #[error("unsupported target provider: {0}")]
    UnsupportedTarget(String),

    #[error("invalid payload for {provider}: {message}")]
    InvalidPayload { provider: String, message: String },

    /// A universal content part cannot be expressed in the target
    /// wire format. M3: Anthropic does not accept `ImageSource::Url`
    /// or `ImageSource::FileId`; the encoder must fail loudly
    /// instead of silently dropping the part.
    #[error("unsupported content for {provider}: {message}")]
    UnsupportedContent { provider: String, message: String },

    #[error("streaming error: {0}")]
    Stream(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("core error: {0}")]
    Core(#[from] CoreError),

    #[error("upstream error: {0}")]
    Upstream(String),

    #[error("http error: {0}")]
    Http(String),

    #[error("config error: {0}")]
    Config(String),
}

impl TranslateError {
    pub fn invalid_payload(provider: &str, msg: impl Into<String>) -> Self {
        Self::InvalidPayload {
            provider: provider.to_string(),
            message: msg.into(),
        }
    }
    pub fn unsupported_content(provider: &str, msg: impl Into<String>) -> Self {
        Self::UnsupportedContent {
            provider: provider.to_string(),
            message: msg.into(),
        }
    }
    pub fn provider(provider: &str, msg: impl Into<String>) -> Self {
        Self::InvalidPayload {
            provider: provider.to_string(),
            message: msg.into(),
        }
    }
    pub fn stream(msg: impl Into<String>) -> Self {
        Self::Stream(msg.into())
    }
    pub fn upstream(msg: impl Into<String>) -> Self {
        Self::Upstream(msg.into())
    }
    pub fn http(msg: impl Into<String>) -> Self {
        Self::Http(msg.into())
    }
    /// Extract the upstream HTTP status code from an `Upstream`
    /// variant's message string, which has the format
    /// `"upstream returned {status}: {snippet}"`. Returns `None`
    /// when the error is not an `Upstream` variant or the message
    /// does not match the expected format.
    pub fn upstream_status(&self) -> Option<u16> {
        match self {
            Self::Upstream(msg) => {
                let rest = msg.strip_prefix("upstream returned ")?;
                let status_str = rest.split(':').next()?;
                status_str.parse::<u16>().ok()
            }
            _ => None,
        }
    }
}
