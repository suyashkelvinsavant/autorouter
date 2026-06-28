//! Error types for the configuration crate.

use thiserror::Error;

pub type ConfigResult<T> = std::result::Result<T, ConfigError>;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("toml parse error: {0}")]
    Toml(#[from] toml::de::Error),

    #[error("toml serialise error: {0}")]
    TomlSer(#[from] toml::ser::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("storage error: {0}")]
    Storage(String),

    #[error("validation error: {0}")]
    Validation(String),

    #[error("secret store error: {0}")]
    Secret(String),

    #[error("not found: {0}")]
    NotFound(String),

    /// N2: list operation is not supported by this backend (e.g.
    /// OS keychains without a portable enumeration API).
    #[error("list not supported: {0}")]
    ListNotSupported(String),
}
