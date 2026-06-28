//! Configuration loader.
//!
//! Precedence:
//! built-in defaults -> system -> user -> env -> runtime override.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{ConfigError, ConfigResult};
use crate::model::AppConfig;
use crate::paths::ProjectPaths;

#[derive(Debug, Clone, Default)]
pub struct ConfigLoader {
    layers: Vec<Layer>,
    override_value: Option<AppConfig>,
}

#[derive(Debug, Clone)]
struct Layer {
    source: LayerSource,
    value: AppConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LayerSource {
    BuiltIn,
    System,
    User,
    Environment,
    Runtime,
}

impl ConfigLoader {
    pub fn new() -> Self {
        Self {
            layers: vec![Layer {
                source: LayerSource::BuiltIn,
                value: AppConfig::default(),
            }],
            override_value: None,
        }
    }

    pub fn load_file(mut self, path: impl AsRef<Path>, source: LayerSource) -> ConfigResult<Self> {
        let path = path.as_ref();
        if !path.exists() {
            tracing::debug!(?path, ?source, "config file missing, skipping");
            return Ok(self);
        }
        let text = std::fs::read_to_string(path)?;
        let value: AppConfig = toml::from_str(&text)?;
        tracing::debug!(?path, ?source, "loaded config layer");
        self.layers.push(Layer { source, value });
        Ok(self)
    }

    pub fn load_environment(mut self) -> Self {
        // H2: start from the most recent layer (not a fresh default)
        // so env vars OVERRIDE earlier file/built-in values, not the
        // other way around.
        let mut value = self
            .layers
            .last()
            .map(|l| l.value.clone())
            .unwrap_or_default();
        if let Ok(v) = std::env::var("AUTOROUTER_BIND") {
            value.server.bind = v;
        }
        if let Ok(v) = std::env::var("AUTOROUTER_LOG_LEVEL") {
            value.logging.level = v;
        }
        if let Ok(v) = std::env::var("AUTOROUTER_LOG_JSON") {
            value.logging.json = Some(matches!(v.as_str(), "1" | "true" | "TRUE" | "yes"));
        }
        if let Ok(v) = std::env::var("AUTOROUTER_DATA_DIR") {
            value.storage.data_dir = v;
        }
        if let Ok(v) = std::env::var("AUTOROUTER_DEFAULT_MODEL") {
            value.defaults.default_model = v;
        }
        if let Ok(v) = std::env::var("AUTOROUTER_DEFAULT_PROVIDER") {
            value.defaults.default_provider = v;
        }
        if let Ok(v) = std::env::var("AUTOROUTER_MAX_TOKENS") {
            if let Ok(n) = v.parse() {
                value.defaults.max_total_tokens = Some(n);
            }
        }
        if let Ok(v) = std::env::var("AUTOROUTER_AUTH_TOKEN") {
            value.server.auth_token = Some(v);
        }
        if let Ok(v) = std::env::var("AUTOROUTER_REQUIRE_AUTH") {
            value.server.require_auth = Some(matches!(v.as_str(), "1" | "true" | "TRUE" | "yes"));
        }
        self.layers.push(Layer {
            source: LayerSource::Environment,
            value,
        });
        self
    }

    pub fn with_override(mut self, value: AppConfig) -> Self {
        self.override_value = Some(value);
        self
    }

    pub fn build(self) -> ConfigResult<AppConfig> {
        let mut merged = AppConfig::default();
        for layer in &self.layers {
            merge_into(&mut merged, &layer.value);
        }
        if let Some(over) = &self.override_value {
            merge_into(&mut merged, over);
        }
        validate(&merged)?;
        Ok(merged)
    }

    pub fn from_standard_chain(roots: &ProjectPaths) -> ConfigResult<AppConfig> {
        // M16: read the first existing config under any of the
        // XDG_CONFIG_DIRS (Linux) / system path (macOS, Windows).
        let user_path = user_config_path(roots);
        let mut loader = Self::new();
        for p in system_config_candidates() {
            loader = loader.load_file(p, LayerSource::System)?;
        }
        loader
            .load_file(user_path, LayerSource::User)?
            .load_environment()
            .build()
    }

    pub fn layer_summary(&self) -> Vec<LayerSource> {
        self.layers.iter().map(|l| l.source).collect()
    }
}

fn system_config_candidates() -> Vec<std::path::PathBuf> {
    #[cfg(target_os = "linux")]
    let mut out: Vec<std::path::PathBuf> = vec![system_config_path()];
    #[cfg(not(target_os = "linux"))]
    let out: Vec<std::path::PathBuf> = vec![system_config_path()];
    #[cfg(target_os = "linux")]
    {
        if let Ok(xdg) = std::env::var("XDG_CONFIG_DIRS") {
            for dir in xdg.split(":").filter(|d| !d.is_empty()) {
                out.push(
                    std::path::PathBuf::from(dir)
                        .join("autorouter")
                        .join("config.toml"),
                );
            }
        }
    }
    out
}

fn system_config_path() -> PathBuf {
    if let Ok(p) = std::env::var("AUTOROUTER_SYSTEM_CONFIG") {
        return PathBuf::from(p);
    }
    #[cfg(target_os = "linux")]
    {
        PathBuf::from("/etc/autorouter/config.toml")
    }
    #[cfg(target_os = "macos")]
    {
        PathBuf::from("/Library/Application Support/autorouter/config.toml")
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(programdata) = std::env::var("PROGRAMDATA") {
            return PathBuf::from(programdata)
                .join("autorouter")
                .join("config.toml");
        }
        PathBuf::from("C:/ProgramData/autorouter/config.toml")
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        PathBuf::new()
    }
}

fn user_config_path(roots: &ProjectPaths) -> PathBuf {
    if let Ok(p) = std::env::var("AUTOROUTER_USER_CONFIG") {
        return PathBuf::from(p);
    }
    roots.config_dir.join("config.toml")
}

fn merge_into(dst: &mut AppConfig, src: &AppConfig) {
    // H1: per-field merge. Each leaf field is only overwritten when the
    // source has a non-default value. This preserves partial overrides
    // from higher-precedence layers without dropping adjacent fields.
    merge_server(&mut dst.server, &src.server);
    merge_defaults(&mut dst.defaults, &src.defaults);
    merge_storage(&mut dst.storage, &src.storage);
    merge_logging(&mut dst.logging, &src.logging);
    // Providers: always overwrite Some(x) with Some(y).
    if src.providers.openai.is_some() {
        dst.providers.openai = src.providers.openai.clone();
    }
    if src.providers.anthropic.is_some() {
        dst.providers.anthropic = src.providers.anthropic.clone();
    }
    if src.providers.gemini.is_some() {
        dst.providers.gemini = src.providers.gemini.clone();
    }
    for (k, v) in &src.providers.custom {
        dst.providers.custom.insert(k.clone(), v.clone());
    }
    // H3: deduplicate routing rules by name. A rule with the same
    // name in a higher-precedence layer replaces the lower one.
    for rule in &src.routing.rules {
        let rule_name = rule.get("name").and_then(|v| v.as_str()).map(String::from);
        if let Some(name) = rule_name {
            if let Some(existing) = dst
                .routing
                .rules
                .iter_mut()
                .find(|r| r.get("name").and_then(|v| v.as_str()) == Some(name.as_str()))
            {
                *existing = rule.clone();
                continue;
            }
        }
        dst.routing.rules.push(rule.clone());
    }
    for tag in &src.routing.default_tags {
        if !dst.routing.default_tags.contains(tag) {
            dst.routing.default_tags.push(tag.clone());
        }
    }
}

fn merge_server(dst: &mut crate::model::ServerConfig, src: &crate::model::ServerConfig) {
    if !src.bind.is_empty() {
        dst.bind = src.bind.clone();
    }
    if src.max_body_bytes != 0 {
        dst.max_body_bytes = src.max_body_bytes;
    }
    if src.request_timeout_seconds != 0 {
        dst.request_timeout_seconds = src.request_timeout_seconds;
    }
    if src.stream_idle_timeout_seconds != 0 {
        dst.stream_idle_timeout_seconds = src.stream_idle_timeout_seconds;
    }
    if let Some(v) = src.require_auth {
        dst.require_auth = Some(v);
    }
    if let Some(v) = src.enable_cors {
        dst.enable_cors = Some(v);
    }
    if let Some(v) = src.allow_target_override_when_unhealthy {
        dst.allow_target_override_when_unhealthy = Some(v);
    }
    if src.auth_token.is_some() {
        dst.auth_token = src.auth_token.clone();
    }
}

fn merge_defaults(dst: &mut crate::model::DefaultsConfig, src: &crate::model::DefaultsConfig) {
    if !src.default_model.is_empty() {
        dst.default_model = src.default_model.clone();
    }
    if !src.default_provider.is_empty() {
        dst.default_provider = src.default_provider.clone();
    }
    if let Some(v) = src.max_total_tokens {
        dst.max_total_tokens = Some(v);
    }
    if let Some(v) = src.stream_by_default {
        dst.stream_by_default = Some(v);
    }
}

fn merge_storage(dst: &mut crate::model::StorageConfig, src: &crate::model::StorageConfig) {
    if !src.data_dir.is_empty() {
        dst.data_dir = src.data_dir.clone();
    }
    if !src.database_file.is_empty() {
        dst.database_file = src.database_file.clone();
    }
    if let Some(v) = src.backup_on_shutdown {
        dst.backup_on_shutdown = Some(v);
    }
    if src.backup_keep != 0 {
        dst.backup_keep = src.backup_keep;
    }
}

fn merge_logging(dst: &mut crate::model::LoggingConfig, src: &crate::model::LoggingConfig) {
    if !src.level.is_empty() {
        dst.level = src.level.clone();
    }
    if let Some(v) = src.json {
        dst.json = Some(v);
    }
    if src.file.is_some() {
        dst.file = src.file.clone();
    }
}

fn validate(config: &AppConfig) -> ConfigResult<()> {
    if config.server.bind.trim().is_empty() {
        return Err(ConfigError::Validation("server.bind is empty".into()));
    }
    // L8: reject malformed bind strings so we fail fast on misconfiguration.
    if let Err(e) = config.server.bind.parse::<std::net::SocketAddr>() {
        return Err(ConfigError::Validation(format!(
            "server.bind is not a valid SocketAddr: {e}"
        )));
    }
    match config.defaults.default_provider.as_str() {
        "openai" | "anthropic" | "gemini" | "" => {}
        other => {
            if !config.providers.custom.contains_key(other) {
                return Err(ConfigError::Validation(format!(
                    "defaults.default_provider references unknown provider `{other}`"
                )));
            }
        }
    }
    if !["error", "warn", "info", "debug", "trace"].contains(&config.logging.level.as_str()) {
        return Err(ConfigError::Validation(format!(
            "logging.level `{}` is not one of error|warn|info|debug|trace",
            config.logging.level
        )));
    }
    if config.server.max_body_bytes < 1024 {
        return Err(ConfigError::Validation(
            "server.max_body_bytes is too small (min 1024)".into(),
        ));
    }
    Ok(())
}
