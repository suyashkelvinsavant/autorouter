//! Serializable configuration model.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub providers: ProvidersConfig,
    pub defaults: DefaultsConfig,
    pub storage: StorageConfig,
    pub logging: LoggingConfig,
    pub routing: RoutingConfig,
    pub features: FeaturesConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub bind: String,
    pub max_body_bytes: usize,
    pub request_timeout_seconds: u64,
    pub stream_idle_timeout_seconds: u64,
    pub enable_cors: Option<bool>,
    pub require_auth: Option<bool>,
    pub auth_token: Option<String>,
    /// M5: when false, a downstream X-AutoRouter-Target override is
    /// ignored if the target provider is below the health threshold
    /// and the router falls back to its health-based choice. Defaults
    /// to false — overrides are blocked by default when the target
    /// provider is unhealthy.
    pub allow_target_override_when_unhealthy: Option<bool>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:4073".into(),
            max_body_bytes: 16 * 1024 * 1024,
            request_timeout_seconds: 300,
            stream_idle_timeout_seconds: 600,
            enable_cors: Some(true),
            require_auth: Some(false),
            auth_token: None,
            allow_target_override_when_unhealthy: Some(false),
        }
    }
}

impl ServerConfig {
    /// CORS is opt-out: the default value is `true`. Single source
    /// of truth for the default so it doesn't have to be re-derived
    /// at every call site (router build, settings patch, headless
    /// binary).
    pub fn cors_enabled(&self) -> bool {
        self.enable_cors.unwrap_or(true)
    }
}

/// Wire format a provider speaks on the outgoing side.
///
/// Detection rule (order matters):
///   1. `api.anthropic.com`                     → `Anthropic`
///   2. `generativelanguage.googleapis.com`     → `Gemini`
///   3. everything else                         → `OpenAI` (the de-facto standard)
///
/// Chinese labs (DeepSeek, Qwen, Yi …), aggregators (OpenRouter,
/// Groq, Together …), and local runtimes (Ollama, LM Studio) all
/// speak OpenAI-compatible; the heuristic is correct for ~99 % of
/// real-world providers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApiFormat {
    #[default]
    OpenAI,
    Anthropic,
    Gemini,
    /// Passthrough mode: raw Responses API bodies are forwarded
    /// directly to the upstream without decode/encode translation.
    Responses,
}

impl std::fmt::Display for ApiFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiFormat::OpenAI => write!(f, "openai"),
            ApiFormat::Anthropic => write!(f, "anthropic"),
            ApiFormat::Gemini => write!(f, "gemini"),
            ApiFormat::Responses => write!(f, "responses"),
        }
    }
}

/// Infer the API format from a provider base URL.
/// Returns `ApiFormat::OpenAI` for any URL not matching the two
/// known non-OpenAI patterns.
pub fn infer_api_format(base_url: &str) -> ApiFormat {
    let lower = base_url.to_ascii_lowercase();
    if lower.contains("anthropic.com") {
        ApiFormat::Anthropic
    } else if lower.contains("googleapis.com") || lower.contains("generativelanguage") {
        ApiFormat::Gemini
    } else {
        ApiFormat::OpenAI
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProvidersConfig {
    pub openai: Option<ProviderEntry>,
    pub anthropic: Option<ProviderEntry>,
    pub gemini: Option<ProviderEntry>,
    #[serde(default)]
    pub custom: BTreeMap<String, ProviderEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderEntry {
    pub display_name: String,
    pub base_url: String,
    pub api_key_secret_id: Option<String>,
    pub default_headers: BTreeMap<String, String>,
    pub enabled: bool,
    #[serde(default)]
    pub model_allowlist: Vec<String>,
    /// Wire format this provider speaks. Auto-detected from `base_url`
    /// when not explicitly set. Persisted to config so operators can
    /// override the heuristic when needed.
    #[serde(default)]
    pub api_format: ApiFormat,
}

impl Default for ProviderEntry {
    fn default() -> Self {
        Self {
            display_name: String::new(),
            base_url: String::new(),
            api_key_secret_id: None,
            default_headers: BTreeMap::new(),
            enabled: true,
            model_allowlist: Vec::new(),
            api_format: ApiFormat::OpenAI,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DefaultsConfig {
    pub default_model: String,
    pub default_provider: String,
    pub stream_by_default: Option<bool>,
    pub max_total_tokens: Option<u32>,
}

impl Default for DefaultsConfig {
    fn default() -> Self {
        Self {
            // Production default: empty. The first provider configured
            // will automatically become the default. This avoids assuming
            // a model the user may not have access to.
            default_model: String::new(),
            default_provider: String::new(),
            stream_by_default: Some(false),
            max_total_tokens: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    pub data_dir: String,
    pub database_file: String,
    pub backup_on_shutdown: Option<bool>,
    pub backup_keep: u32,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            data_dir: String::new(),
            database_file: "autorouter.db".into(),
            backup_on_shutdown: Some(true),
            backup_keep: 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LoggingConfig {
    pub level: String,
    pub json: Option<bool>,
    pub file: Option<String>,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".into(),
            json: Some(false),
            file: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RoutingConfig {
    #[serde(default)]
    pub rules: Vec<serde_json::Value>,
    #[serde(default)]
    pub default_tags: Vec<String>,
}

/// Opt-in feature flags. Each toggle controls a piece of
/// background behaviour that touches the network or the filesystem
/// beyond the core translate-and-forward loop. All default to **off**
/// so a fresh install never makes unexpected outbound connections.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FeaturesConfig {
    /// When `true`, the gateway periodically scrapes
    /// `openrouter.ai/api/v1/models` and
    /// `artificialanalysis.ai/leaderboards/models` to enrich the
    /// built-in model database with pricing, context-window, and
    /// benchmark data. **Off by default** — a local-first app should
    /// not phone home without the operator's consent. The scraped JSON
    /// is cached at `<data_dir>/models_data.json` and refreshed at most
    /// once per 24 hours.
    pub model_scraping: bool,
}
