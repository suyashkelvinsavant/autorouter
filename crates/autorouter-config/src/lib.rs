#![deny(unused_crate_dependencies)]
//! autorouter-config
//!
//! Configuration loader, secret store, and SQLite-backed persistent
//! state for AutoRouter.

// Declared in [dev-dependencies]; used only in integration tests.
#[cfg(test)]
use tempfile as _;
pub mod api_key;
pub mod error;
pub mod loader;
pub mod model;
pub mod paths;
pub mod secret;
pub mod storage;

pub use api_key::{
    classify_api_key_reference, looks_like_api_key, looks_like_env_var_name, ApiKeyReference,
};
pub use error::{ConfigError, ConfigResult};
pub use loader::{ConfigLoader, LayerSource};
pub use model::{
    infer_api_format, ApiFormat, AppConfig, DefaultsConfig, LoggingConfig, ProviderEntry,
    ProvidersConfig, RoutingConfig, ServerConfig, StorageConfig,
};
pub use paths::ProjectPaths;
pub use secret::{
    build_secret_store, FileStore, InMemoryStore, KeyringStore, Secret, SecretId, SecretStore,
};
pub use storage::{PersistedSession, ProviderEvent, Storage, SCHEMA_VERSION};
