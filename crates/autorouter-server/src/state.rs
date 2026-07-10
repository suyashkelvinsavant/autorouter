//! Application state shared by all route handlers.
//!
//! This module also exposes [`build_smart_router`] and
//! [`user_config_path`] helpers used by both the headless and the
//! desktop binaries.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use autorouter_config::{AppConfig, ProjectPaths};
use autorouter_core::ProviderKind;
use autorouter_router::{
    CapabilityRegistry, HealthTracker, IdentityRouter, ModelCapability, RouteTarget, Router,
    RoutingRule, RuleEngine, SmartRouter,
};
use autorouter_translate::{ProviderAdapter, TranslationPipeline};

use crate::model_db::ModelDb;
use crate::session::SessionRegistry;
use crate::storage::StorageHandle;
use crate::upstream::{SharedUpstream, UpstreamSet};

/// Cloned cheaply and shared by every request handler.
#[derive(Clone)]
pub struct AppState {
    pub model_db: Arc<parking_lot::RwLock<ModelDb>>,
    /// On-disk data directory for persistent files (model DB cache,
    /// backups). Threaded from the `ProjectPaths` resolved by the
    /// process entry point so no handler ever needs to read an env
    /// var directly (AGENTS.md hard rule: "No ad-hoc std::env::var
    /// outside autorouter-config").
    pub data_dir: Arc<std::path::PathBuf>,
    /// Live configuration. Wrapped in RwLock so /ui/settings PATCH
    /// can swap in a new config and every future request sees the
    /// change (require_auth, CORS, timeouts, defaults, etc).
    pub config: Arc<parking_lot::RwLock<Arc<AppConfig>>>,
    pub pipeline: Arc<TranslationPipeline>,
    /// Atomic upstream map (built-in providers + custom providers).
    ///
    /// Wrapped in `parking_lot::RwLock<UpstreamSet>` so that
    /// `PATCH /ui/settings` can rebuild the entire upstream set from
    /// the patched config and swap it in atomically. In-flight
    /// requests that already cloned a `SharedUpstream` keep using
    /// the old client until they finish; new requests use the new
    /// client. This is the fix for "I changed the API key in the
    /// dashboard and the gateway kept using the old key".
    pub upstreams: Arc<parking_lot::RwLock<UpstreamSet>>,
    pub sessions: SessionRegistry,
    /// The smart router used to pick an upstream target. May be the
    /// identity router in dev/headless mode.
    ///
    /// Wrapped in `parking_lot::RwLock` so `PATCH /ui/routing`,
    /// `PATCH /ui/settings` (when the default target changes), and
    /// the desktop Tauri `cmd_routing_patch` /
    /// `cmd_settings_patch` commands can swap in a rebuilt router on
    /// the next request — without waiting for a process restart.
    /// Without this wrapper the gateway would keep routing with the
    /// rules that were loaded at startup time.
    pub router: Arc<parking_lot::RwLock<Arc<dyn Router>>>,
    /// Rolling health samples per provider, used by the smart router
    /// for health-based fallbacks and surfaced on the dashboard.
    pub health: Arc<HealthTracker>,
    /// Optional SQLite-backed storage. When present, every upstream
    /// call records a provider_events row and the gateway can
    /// persist + restore settings across restarts.
    pub storage: Option<Arc<StorageHandle>>,
}

impl AppState {
    pub fn new(
        config: AppConfig,
        pipeline: TranslationPipeline,
        upstreams: HashMap<ProviderKind, SharedUpstream>,
    ) -> Self {
        Self::with_router(config, pipeline, upstreams, None, HealthTracker::new())
    }

    /// Build an `AppState` with an explicit router. When `router` is
    /// `None`, an [`IdentityRouter`](autorouter_router::IdentityRouter)
    /// is used and the supplied `health` tracker is attached.
    pub fn with_router(
        config: AppConfig,
        pipeline: TranslationPipeline,
        upstreams: HashMap<ProviderKind, SharedUpstream>,
        router: Option<Arc<dyn Router>>,
        health: HealthTracker,
    ) -> Self {
        let resolved_router: Arc<dyn Router> = match router {
            Some(r) => r,
            None => {
                let adapters: Vec<Arc<dyn ProviderAdapter>> = pipeline_adapters(&pipeline);
                Arc::new(IdentityRouter::new(adapters))
            }
        };
        tracing::warn!(data_dir = "data", "AppState constructed without data_dir; falling back to relative path — call with_data_dir()");
        Self {
            model_db: Arc::new(parking_lot::RwLock::new(ModelDb::bundled_defaults())),
            data_dir: Arc::new(std::path::PathBuf::from("data")),
            config: Arc::new(parking_lot::RwLock::new(Arc::new(config))),
            pipeline: Arc::new(pipeline),
            upstreams: Arc::new(parking_lot::RwLock::new(UpstreamSet {
                built_in: upstreams,
                custom: BTreeMap::new(),
            })),
            sessions: SessionRegistry::new(), // H13: switch to SessionRegistry::with_storage when storage is present (see with_storage helper below)
            router: Arc::new(parking_lot::RwLock::new(resolved_router)),
            health: Arc::new(health),
            storage: None,
        }
    }

    /// Snapshot of registered adapters, used by the model-listing
    /// endpoint.
    pub fn pipeline_adapters(&self) -> Vec<Arc<dyn ProviderAdapter>> {
        pipeline_adapters(&self.pipeline)
    }

    pub fn with_model_db(mut self, model_db: Arc<parking_lot::RwLock<ModelDb>>) -> Self {
        self.model_db = model_db;
        self
    }

    /// Set the on-disk data directory used for the model DB cache and
    /// backups. Should be called by every entry point with the
    /// `paths.data_dir` resolved via `ProjectPaths`.
    pub fn with_data_dir(mut self, data_dir: std::path::PathBuf) -> Self {
        self.data_dir = Arc::new(data_dir);
        self
    }

    /// Attach a [StorageHandle] so the gateway can record
    /// provider events, persist runtime settings, and persist
    /// sessions (H13).
    pub fn with_storage(mut self, storage: Option<Arc<StorageHandle>>) -> Self {
        if let Some(handle) = storage.as_ref() {
            self.sessions = SessionRegistry::with_storage(handle.clone());
        }
        self.storage = storage;
        self
    }

    /// Attach custom upstreams so the gateway can serve
    /// providers.custom.<name> from config.
    pub fn with_custom_upstreams(self, custom: BTreeMap<String, SharedUpstream>) -> Self {
        // Mutate the inner UpstreamSet in place rather than replacing
        // the outer Arc so any outstanding State<AppState> clones
        // continue to see the same lock. Rebuild via
        // `replace_upstreams` if you need to swap both maps
        // atomically.
        self.upstreams.write().custom = custom;
        self
    }

    /// Replace the shared config with a new one. All future
    /// request handlers will see the updated values (auth, CORS,
    /// timeouts, defaults, etc). Used by the PATCH /ui/settings
    /// path so toggling `require_auth` takes effect immediately.
    pub fn replace_config(&self, new_config: autorouter_config::AppConfig) {
        *self.config.write() = Arc::new(new_config);
    }

    /// Read the current shared config. All readers see the latest
    /// swapped-in config.
    pub fn current_config(&self) -> Arc<AppConfig> {
        self.config.read().clone()
    }

    /// Look up the built-in upstream client for `kind` and clone the
    /// shared handle. The returned `SharedUpstream` keeps the old
    /// client alive even if the gateway rebuilds the upstream set
    /// before this request finishes, which is exactly what we want
    /// for "in-flight requests keep their old client" semantics.
    pub fn upstream_for(&self, kind: ProviderKind) -> Option<SharedUpstream> {
        self.upstreams.read().built_in.get(&kind).cloned()
    }

    /// Look up a custom upstream client by name. Same atomicity
    /// guarantees as [`Self::upstream_for`].
    pub fn custom_upstream_for(&self, name: &str) -> Option<SharedUpstream> {
        self.upstreams.read().custom.get(name).cloned()
    }

    /// Atomically replace the entire upstream set. Existing callers
    /// that already cloned a `SharedUpstream` continue to use the
    /// old client; new callers see the new clients. The RwLock
    /// inside `UpstreamSet` is taken in write mode for the brief
    /// moment of the swap; readers block only for the lock
    /// acquisition.
    ///
    /// Why this needs to be public: the HTTP `PATCH /ui/settings`
    /// handler in `crate::ui` calls this after applying the patch
    /// so that secrets / base URLs / enabled toggles take effect
    /// on the next request without a gateway restart.
    pub fn replace_upstreams(&self, new_set: UpstreamSet) {
        *self.upstreams.write() = new_set;
    }

    /// Snapshot the current upstream set. Used by tests and by
    /// the `PATCH /ui/settings` handler to compute the new set.
    pub fn snapshot_upstreams(&self) -> UpstreamSet {
        self.upstreams.read().clone()
    }

    /// Snapshot of the current smart router. The router is wrapped
    /// in an `Arc` so callers can hold a clone and release the lock
    /// before calling `decide` (the lock is only needed to swap the
    /// router out, not to read it for routing decisions).
    pub fn current_router(&self) -> Arc<dyn Router> {
        self.router.read().clone()
    }

    /// Atomically swap in a new router. Existing in-flight requests
    /// that already cloned the old `Arc<dyn Router>` keep using the
    /// previous rules; new requests see the new ones. The atomic
    /// swap guarantees the gateway never observes a half-built
    /// router. Called from `PATCH /ui/routing`,
    /// `PATCH /ui/settings` (when the default target changes), and
    /// the desktop Tauri routing/settings patch commands.
    ///
    /// Why this needs to be public: same reason as
    /// [`Self::replace_upstreams`] — the HTTP handlers need a way
    /// to refresh the in-process routing engine without restarting
    /// the gateway.
    pub fn replace_router(&self, new_router: Arc<dyn Router>) {
        *self.router.write() = new_router;
    }
}

fn pipeline_adapters(pipeline: &TranslationPipeline) -> Vec<Arc<dyn ProviderAdapter>> {
    [
        ProviderKind::OpenAI,
        ProviderKind::Anthropic,
        ProviderKind::Gemini,
    ]
    .iter()
    .filter_map(|k| pipeline.adapter_for(*k).ok())
    .collect()
}

pub fn build_smart_router(
    pipeline: &TranslationPipeline,
    config: &AppConfig,
    health: HealthTracker,
    model_db: &ModelDb,
) -> Arc<SmartRouter> {
    let mut reg = CapabilityRegistry::new();
    for adapter in pipeline_adapters(pipeline) {
        for m in adapter.models() {
            let scraped = model_db.get(&m.id);
            let input_price = scraped.map(|s| s.input_price_per_million);
            let output_price = scraped.map(|s| s.output_price_per_million);
            let is_free = scraped
                .map(|s| s.is_free)
                .unwrap_or_else(|| lookup_pricing_free(&m.id));

            reg.register(ModelCapability {
                provider: m.provider,
                model: m.id.clone(),
                context_window: m.context_window,
                max_output_tokens: m.max_output_tokens,
                supports_tools: m.supports_tools,
                supports_vision: m.supports_vision,
                supports_audio: m.supports_audio,
                supports_streaming: m.supports_streaming,
                input_price_per_million: input_price,
                output_price_per_million: output_price,
                is_free,
            });
        }
    }
    for entry in config.providers.custom.values() {
        for model_id in &entry.model_allowlist {
            let scraped = model_db.get(model_id);
            let input_price = scraped.map(|s| s.input_price_per_million);
            let output_price = scraped.map(|s| s.output_price_per_million);
            let is_free = scraped
                .map(|s| s.is_free)
                .unwrap_or_else(|| lookup_pricing_free(model_id));
            // Use scraped context_length when available; fall back to a
            // conservative default. max_output_tokens is not scraped so
            // keep a 4096 default (safe for all known providers).
            let context_window = scraped
                .map(|s| s.context_length)
                .filter(|&v| v > 0)
                .unwrap_or(131072);

            reg.register(ModelCapability {
                provider: ProviderKind::Custom,
                model: model_id.clone(),
                context_window,
                max_output_tokens: 4096,
                supports_tools: true,
                supports_vision: true,
                supports_audio: false,
                supports_streaming: true,
                input_price_per_million: input_price,
                output_price_per_million: output_price,
                is_free,
            });
        }
    }
    let mut rules = RuleEngine::new();
    let default_provider =
        provider_kind_from_str_or_openai(&config.defaults.default_provider, config);
    let default_target = RouteTarget {
        provider: default_provider,
        model: config.defaults.default_model.clone(),
        headers: Default::default(),
    };
    for raw in &config.routing.rules {
        if let Ok(rule) = serde_json::from_value::<RoutingRule>(raw.clone()) {
            rules.add_rule(rule);
        }
    }
    rules.add_rule(RoutingRule::default_rule(default_target));
    Arc::new(SmartRouter::new(rules, reg, health).with_min_health(0.1))
}

fn provider_kind_from_str_or_openai(s: &str, config: &AppConfig) -> ProviderKind {
    match s.to_ascii_lowercase().as_str() {
        "openai" => ProviderKind::OpenAI,
        "anthropic" | "claude" => ProviderKind::Anthropic,
        "gemini" | "google" => ProviderKind::Gemini,
        "custom" => ProviderKind::Custom,
        _ => {
            if config.providers.custom.contains_key(s) {
                ProviderKind::Custom
            } else {
                ProviderKind::OpenAI
            }
        }
    }
}

/// Resolve the user-level config file path. Thin wrapper around
/// `ConfigLoader`'s internal helper exposed for the desktop and the
/// `UiState`.
pub fn user_config_path(paths: &ProjectPaths) -> PathBuf {
    if let Ok(p) = std::env::var("AUTOROUTER_USER_CONFIG") {
        return PathBuf::from(p);
    }
    paths.config_dir.join("config.toml")
}

/// H12: data-driven pricing table. The model DB is the primary
/// pricing source; this table supplies the `is_free` fallback for
/// models the DB does not yet know about (a name-pattern heuristic:
/// `flash` / `mini` / `free` are treated as free).
fn lookup_pricing_free(model_id: &str) -> bool {
    for (needle, _, _, free) in PRICING_TABLE {
        if model_id.to_ascii_lowercase().contains(needle) {
            return *free;
        }
    }
    // Heuristic fallback for unknown models.
    let lower = model_id.to_ascii_lowercase();
    lower.contains("flash") || lower.contains("mini") || lower.contains("free")
}

const PRICING_TABLE: &[(&str, f64, f64, bool)] = &[
    ("gpt-5", 5.0, 15.0, false),
    ("gpt-5-mini", 0.25, 2.0, false),
    ("gpt-4o", 2.5, 10.0, false),
    ("gpt-4o-mini", 0.15, 0.6, false),
    ("o3", 10.0, 40.0, false),
    ("o3-mini", 1.1, 4.4, false),
    ("claude-opus-4-5", 15.0, 75.0, false),
    ("claude-sonnet-4-5", 3.0, 15.0, false),
    ("claude-haiku-4-5", 0.8, 4.0, false),
    ("gemini-2.5-pro", 1.25, 10.0, false),
    ("gemini-2.5-flash", 0.0, 0.0, true),
];
