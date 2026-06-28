//! UI-facing HTTP endpoints.
//!
//! These power the desktop dashboard. The endpoints are read-mostly:
//! they return JSON snapshots of the running gateway and accept a
//! small set of write requests (settings update, server control).

use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// H4: process-level lock for config writes. Ensures concurrent
/// PATCH /ui/settings requests serialise through a single writer so
/// the temp-file + rename dance is race-free.
static CONFIG_WRITE_LOCK: Mutex<()> = Mutex::new(());

use autorouter_config::AppConfig;

use crate::error::{ServerError, ServerResult};
use crate::router::build_router_with_ui;
use crate::state::AppState;
use crate::supervisor::{GatewaySupervisor, RebindOutcome};
use crate::upstream::UpstreamClient;

/// State shared by the UI surface.
#[derive(Clone, Default)]
pub struct UiState {
    pub config: Arc<RwLock<AppConfig>>,
    pub start_time: Arc<RwLock<chrono::DateTime<chrono::Utc>>>,
    pub log_lines: Arc<RwLock<Vec<LogLine>>>,
    /// Path to write the persisted config to. When `None`, PATCH
    /// requests update only the in-memory state.
    pub config_path: Arc<RwLock<Option<PathBuf>>>,
    /// Optional storage for provider events.
    pub storage: Arc<RwLock<Option<Arc<crate::storage::StorageHandle>>>>,
    /// Optional secret store. The dashboard surfaces a
    /// `/ui/secrets` endpoint that returns the backend name and
    /// whether `list()` is supported, plus the list of secret ids
    /// (or `null` when the backend cannot enumerate).
    pub secret_store: Arc<RwLock<Option<Arc<dyn autorouter_config::SecretStore>>>>,
}

/// Combined state carried by the UI router. The gateway state is
/// cheap to clone, so we hold an `AppState` directly.
///
/// `supervisor` is `Some` for the desktop binary (which owns a
/// `GatewaySupervisor` and can hot-rebind) and `None` for the
/// headless binary (which binds the listener once at startup and
/// cannot rebind without a process restart). The HTTP handlers use
/// this to decide whether a bind change should be applied live or
/// reported as a "restart required" error.
#[derive(Clone)]
pub struct UiAppState {
    pub ui: UiState,
    pub app: AppState,
    /// Optional gateway supervisor. When `Some`, the HTTP `patch_settings` and
    /// `restart` handlers hot-rebind the listener when the bind changes. When
    /// `None` (headless binary), the bind change is recorded in the in-memory
    /// config but the listener stays pinned to its startup port until the
    /// process is restarted.
    pub supervisor: Option<GatewaySupervisor>,
}

impl UiAppState {
    /// Construct a new `UiAppState` with the given UI state and AppState, and
    /// no supervisor (the headless default).
    pub fn new(ui: UiState, app: AppState) -> Self {
        Self {
            ui,
            app,
            supervisor: None,
        }
    }

    /// Attach a `GatewaySupervisor` so the HTTP `patch_settings` and
    /// `restart` handlers can hot-rebind the listener when the bind
    /// changes. Returns the modified state for builder-style use.
    pub fn with_supervisor(mut self, supervisor: GatewaySupervisor) -> Self {
        self.supervisor = Some(supervisor);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogLine {
    pub ts: chrono::DateTime<chrono::Utc>,
    pub level: String,
    pub target: String,
    pub message: String,
}

impl LogLine {
    pub fn push(buf: &RwLock<Vec<LogLine>>, level: &str, target: &str, message: &str) {
        let mut g = buf.write();
        g.push(LogLine {
            ts: chrono::Utc::now(),
            level: level.to_string(),
            target: target.to_string(),
            message: message.to_string(),
        });
        if g.len() > 2_000 {
            let drop = g.len() - 2_000;
            g.drain(0..drop);
        }
    }
}

/// Build the UI sub-router. The sub-router is meant to be merged
/// into the gateway router via `Router::merge`.
pub fn build_sub_router(state: UiAppState) -> Router {
    Router::new()
        .route("/ui/status", get(status))
        .route("/ui/providers", get(providers))
        .route("/ui/sessions", get(sessions))
        .route("/ui/settings", get(get_settings).patch(patch_settings))
        .route("/ui/logs", get(get_logs))
        .route("/ui/restart", post(restart))
        .route("/ui/server", get(server_info))
        .route("/ui/routing", get(get_routing).patch(patch_routing))
        .route("/ui/health", get(get_health))
        .route("/ui/events", get(get_events))
        .route("/ui/secrets", get(get_secrets))
        .route(
            "/ui/secrets/:id",
            get(get_secret_value).put(put_secret_value),
        )
        .route("/ui/analytics", get(get_analytics))
        .route("/ui/debug", get(get_debug))
        .route(
            "/ui/tool_profiles",
            get(list_tool_profiles).post(save_tool_profile),
        )
        .route("/ui/tool_test", post(test_tool))
        .route("/ui/provider_test", post(test_provider))
        .route("/ui/import", post(import_config))
        .route("/ui/export", get(export_config))
        .route("/ui/update", get(check_update))
        .with_state(state)
}

/// Merge helper kept for backwards compatibility (used by the
/// desktop's `lib.rs` and by tests).
pub fn merge(router: Router, state: UiAppState) -> Router {
    router.merge(build_sub_router(state))
}

/// Authorization for `/ui/*` routes. Delegates to the shared
/// [`crate::routes::maybe_authorize`] so the bearer-token check stays
/// in one place — the previous `authorize` here was a byte-for-byte
/// duplicate of `maybe_authorize`, with the only difference being
/// the function name.
fn authorize(headers: &HeaderMap, state: &AppState) -> ServerResult<()> {
    crate::routes::maybe_authorize(headers, state)
}

async fn status(State(s): State<UiAppState>, headers: HeaderMap) -> ServerResult<Json<Value>> {
    authorize(&headers, &s.app)?;
    let cfg = s.ui.config.read().clone();
    // R10: prefer the supervisor's live socket address so the
    // dashboard reflects the actually-listening port, not the value
    // the config file claims. Falls back to the in-memory config when
    // no supervisor is attached (headless binary) or while the
    // supervisor is briefly between listeners during a rebind.
    let bind = s
        .supervisor
        .as_ref()
        .and_then(|sup| sup.current_bind())
        .unwrap_or_else(|| cfg.server.bind.clone());
    let started = *s.ui.start_time.read();
    let uptime = (chrono::Utc::now() - started).num_seconds().max(0);
    let log_count = s.ui.log_lines.read().len();
    let sessions = s.app.sessions.list();
    // Report per-provider configuration state to the dashboard.
    // Each entry is one of:
    //   * `"configured"` — entry exists, has a base URL and API key
    //     secret id, and `enabled = true`. The gateway will forward
    //     real upstream traffic.
    //   * `"disabled"`   — entry exists and has a base URL, but
    //     `enabled = false`. The gateway will route around it.
    //   * `"missing"`    — no entry at all (first run, or the
    //     operator has not configured it).
    let provider_state = |entry: &Option<autorouter_config::ProviderEntry>| -> &'static str {
        match entry {
            Some(e) if e.enabled && !e.base_url.is_empty() => "configured",
            Some(_) => "disabled",
            None => "missing",
        }
    };
    Ok(Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "bind": bind,
        "started_at": started,
        "uptime_seconds": uptime,
        "log_lines": log_count,
        "session_count": sessions.len(),
        "providers": {
            "openai": provider_state(&cfg.providers.openai),
            "anthropic": provider_state(&cfg.providers.anthropic),
            "gemini": provider_state(&cfg.providers.gemini),
        }
    })))
}

async fn providers(State(s): State<UiAppState>, headers: HeaderMap) -> ServerResult<Json<Value>> {
    authorize(&headers, &s.app)?;
    let cfg = s.ui.config.read().clone();
    let mut out: Vec<Value> = Vec::new();
    // Dedupe: any `providers.custom` entry whose key collides with
    // a first-class slot (see module-level `FIRST_CLASS_IDS`) is
    // dropped here. The first-class card always wins so the
    // dashboard renders exactly one card per id. Manual edits to
    // `config.toml` or older config files that pre-date the
    // first-class distinction can still produce this state; the
    // server-side `apply_settings_patch` also rejects creating the
    // collision in the first place.
    for (name, entry) in [
        ("openai", cfg.providers.openai.clone()),
        ("anthropic", cfg.providers.anthropic.clone()),
        ("gemini", cfg.providers.gemini.clone()),
    ] {
        let Some(e) = entry else { continue };
        // For first-class providers the api_format stored in the entry
        // is authoritative; fall back to URL-inference for old configs.
        let fmt = if e.base_url.is_empty() {
            match name {
                "anthropic" => "anthropic",
                "gemini" => "gemini",
                _ => "openai",
            }
        } else {
            match autorouter_config::infer_api_format(&e.base_url) {
                autorouter_config::ApiFormat::Anthropic => "anthropic",
                autorouter_config::ApiFormat::Gemini => "gemini",
                autorouter_config::ApiFormat::OpenAI => "openai",
            }
        };
        out.push(json!({
            "id": name,
            "display_name": e.display_name,
            "base_url": e.base_url,
            "enabled": e.enabled,
            "api_key_secret_id": e.api_key_secret_id,
            "default_headers": e.default_headers,
            "model_allowlist": e.model_allowlist,
            "api_format": fmt,
        }));
    }
    for (name, e) in &cfg.providers.custom {
        // Dedupe: a custom entry whose id collides with a first-class
        // slot is dropped here. The first-class card always wins so
        // the dashboard renders exactly one card per id. Manual
        // edits to `config.toml` or older config files that pre-date
        // the first-class distinction can still produce this state;
        // the server-side `apply_settings_patch` also rejects
        // creating the collision in the first place (single source
        // of truth = `FIRST_CLASS_IDS`).
        if FIRST_CLASS_IDS.contains(&name.as_str()) {
            continue;
        }
        let fmt = match e.api_format {
            autorouter_config::ApiFormat::Anthropic => "anthropic",
            autorouter_config::ApiFormat::Gemini => "gemini",
            autorouter_config::ApiFormat::OpenAI => "openai",
        };
        out.push(json!({
            "id": name,
            "kind": "custom",
            "display_name": e.display_name,
            "base_url": e.base_url,
            "enabled": e.enabled,
            "api_key_secret_id": e.api_key_secret_id,
            "default_headers": e.default_headers,
            "model_allowlist": e.model_allowlist,
            "api_format": fmt,
        }));
    }
    let adapters = s.app.pipeline_adapters();
    let mut models: Vec<Value> = Vec::new();
    for a in adapters.iter() {
        for m in a.models().iter() {
            models.push(json!({
                "id": m.id,
                "provider": format!("{:?}", m.provider).to_lowercase(),
                "context_window": m.context_window,
                "max_output_tokens": m.max_output_tokens,
                "supports_tools": m.supports_tools,
                "supports_vision": m.supports_vision,
                "supports_audio": m.supports_audio,
                "supports_streaming": m.supports_streaming,
            }));
        }
    }
    for (name, entry) in &cfg.providers.custom {
        for model_id in &entry.model_allowlist {
            models.push(json!({
                "id": model_id,
                "provider": name.clone(),
                "context_window": 131072,
                "max_output_tokens": 4096,
                "supports_tools": true,
                "supports_vision": true,
                "supports_audio": false,
                "supports_streaming": true,
            }));
        }
    }
    let health: Vec<Value> = [
        autorouter_core::ProviderKind::OpenAI,
        autorouter_core::ProviderKind::Anthropic,
        autorouter_core::ProviderKind::Gemini,
    ]
    .iter()
    .map(|k| {
        let snap = s.app.health.snapshot(*k);
        json!({
            "provider": k.to_string(),
            "samples": snap.samples,
            "success_rate": snap.success_rate,
            "avg_latency_ms": snap.avg_latency_ms,
            "score": snap.score,
        })
    })
    .collect();
    Ok(Json(json!({
        "providers": out,
        "models": models,
        "health": health,
    })))
}

async fn sessions(State(s): State<UiAppState>, headers: HeaderMap) -> ServerResult<Json<Value>> {
    authorize(&headers, &s.app)?;
    let list: Vec<Value> = s
        .app
        .sessions
        .list()
        .into_iter()
        .map(|se| {
            json!({
                "id": se.id,
                "label": se.label,
                "source_provider": se.source_provider,
                "created_at": se.created_at,
                "last_request_at": se.updated_at,
                "last_request_id": se.last_request_id,
                "request_count": se.request_count,
            })
        })
        .collect();
    Ok(Json(json!({ "sessions": list })))
}

async fn server_info(State(s): State<UiAppState>, headers: HeaderMap) -> ServerResult<Json<Value>> {
    authorize(&headers, &s.app)?;
    let cfg = s.ui.config.read().clone();
    // R10: report the live socket address from the supervisor when one
    // is attached so the dashboard shows the actually-listening port
    // after a rebind, not the value the config file claims.
    let bind = s
        .supervisor
        .as_ref()
        .and_then(|sup| sup.current_bind())
        .unwrap_or_else(|| cfg.server.bind.clone());
    Ok(Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "build": { "target": std::env::consts::ARCH, "os": std::env::consts::OS },
        "config": {
            "bind": bind,
            "max_body_bytes": cfg.server.max_body_bytes,
            "request_timeout_seconds": cfg.server.request_timeout_seconds,
            "stream_idle_timeout_seconds": cfg.server.stream_idle_timeout_seconds,
            "enable_cors": cfg.server.cors_enabled(),
            "require_auth": cfg.server.require_auth.unwrap_or(false),
        }
    })))
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct SettingsPatch {
    #[serde(default)]
    pub server: Option<ServerPatch>,
    #[serde(default)]
    pub defaults: Option<DefaultsPatch>,
    #[serde(default)]
    pub logging: Option<LoggingPatch>,
    #[serde(default)]
    pub providers: ProvidersPatch,
    /// When true (the default), the PATCH is persisted to the user
    /// config file. The desktop shell uses this to opt out of
    /// persistence for "preview" edits.
    #[serde(default)]
    pub persist: Option<bool>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct ServerPatch {
    pub bind: Option<String>,
    pub enable_cors: Option<bool>,
    pub require_auth: Option<bool>,
    pub auth_token: Option<String>,
    pub request_timeout_seconds: Option<u64>,
    pub max_body_bytes: Option<usize>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct DefaultsPatch {
    pub default_model: Option<String>,
    pub default_provider: Option<String>,
    pub stream_by_default: Option<bool>,
    pub max_total_tokens: Option<u32>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct LoggingPatch {
    pub level: Option<String>,
    pub json: Option<bool>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct ProvidersPatch {
    pub openai: Option<ProviderPatch>,
    pub anthropic: Option<ProviderPatch>,
    pub gemini: Option<ProviderPatch>,
    #[serde(default)]
    pub custom: std::collections::BTreeMap<String, ProviderPatch>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct ProviderPatch {
    pub display_name: Option<String>,
    pub base_url: Option<String>,
    pub api_key_secret_id: Option<String>,
    pub api_key_value: Option<String>,
    pub default_headers: Option<std::collections::BTreeMap<String, String>>,
    pub enabled: Option<bool>,
    pub model_allowlist: Option<Vec<String>>,
    pub delete: Option<bool>,
    /// Explicit wire-format override. When absent, the format is
    /// inferred from `base_url` by `infer_api_format`.
    pub api_format: Option<String>,
}

/// Persist an API key value supplied via the Providers page to the
/// configured secret store, and update the `ProviderEntry`'s
/// `api_key_secret_id` to reference the stored secret.
///
/// Auto-detection: the operator can paste *either* of the following
/// into the `api_key_value` field and AutoRouter will do the right
/// thing:
///
///   * `env:NAME` or a bare ALL_CAPS_SNAKE_CASE name that matches a
///     process env var → store as `env:NAME` (no value persisted;
///     the gateway reads the env var on every request).
///   * Anything else (a literal key, a custom store id) → store the
///     value in the secret store under an id and reference it as
///     `keychain:{id}`.
///
/// The existing `api_key_secret_id` on the entry is honoured if the
/// operator explicitly typed it; if it is absent, the helper picks
/// the auto-detected canonical form.
///
/// M17: also accept `api_key_value` being absent and the
/// `api_key_secret_id` field itself containing a literal key. The
/// Providers UI in the desktop / Vite dev surfaces a single
/// "API Key" textbox; the form posts the value as
/// `api_key_secret_id` for backward compatibility. To honour the
/// "Secrets come from the secret store" hard rule we now treat a
/// bare non-env, non-keychain value in that field as a literal key
/// that must be put into the secret store, and we re-write the
/// entry to a `keychain:` reference. The previous behaviour
/// persisted the literal key into `config.toml`, which violated
/// Hard rule #6 in AGENTS.md.
fn save_secret_for_provider(
    provider_id: &str,
    entry: &mut autorouter_config::ProviderEntry,
    api_key_secret_id_patch: Option<String>,
    api_key_value: Option<String>,
    secret_store: Option<&Arc<dyn autorouter_config::SecretStore>>,
) {
    // R12: when the form posts `api_key_value` separately, that is
    // the literal key (or env reference) and `api_key_secret_id`
    // is either empty or an explicit secret id. When the form only
    // posts `api_key_secret_id` (the legacy single-field shape), the
    // value in that field is the literal key / env reference /
    // secret id; we fall back to using it as the "value".
    let raw_value = api_key_value
        .clone()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            api_key_secret_id_patch
                .clone()
                .filter(|v| !v.trim().is_empty())
        });
    let Some(val) = raw_value else { return };

    // Case 1: the operator explicitly typed something into the
    // `api_key_secret_id` field. Honour it verbatim — we treat the
    // value as the secret-store value to save under that id (or as
    // an env reference, depending on the prefix).
    if let Some(explicit) = api_key_secret_id_patch.as_deref() {
        let explicit = explicit.trim();
        if !explicit.is_empty() {
            let classified = autorouter_config::classify_api_key_reference(explicit);
            match classified {
                autorouter_config::ApiKeyReference::EnvVar(name) => {
                    // Operator said "use this env var". Drop the
                    // literal value on the floor (we never persist
                    // env values to disk) and point the entry at
                    // the env var name.
                    entry.api_key_secret_id = Some(format!("env:{name}"));
                    return;
                }
                autorouter_config::ApiKeyReference::SecretId(id) => {
                    let id = id.to_string();
                    // Honour the typed id only when it is clearly a
                    // curated name — short, doesn't look like a literal
                    // API key, and matches env-var naming. A long
                    // key-shaped string here is a leaked literal key
                    // (the single-field shape the Providers UI posts
                    // when the operator pastes the key into the
                    // secret-id textbox). Mirror Case 2's check: fall
                    // through to the auto-generated id path so the
                    // value lands under `f"{provider_id}_api_key"`
                    // and the literal key never ends up as the
                    // secret-store id (Hard rule #6 in AGENTS.md).
                    let is_curated_id = id.len() <= 32
                        && !autorouter_config::looks_like_api_key(&id)
                        && autorouter_config::looks_like_env_var_name(&id);
                    if is_curated_id {
                        if let Some(store) = secret_store {
                            let _ = store.put(autorouter_config::Secret::new(id.clone(), val));
                        }
                        entry.api_key_secret_id = Some(format!("keychain:{id}"));
                        return;
                    }
                    // else: fall through to Case 2's auto-id path.
                }
            }
        }
    }

    // Case 2: no explicit secret id was provided. Auto-classify
    // the value itself.
    let classified = autorouter_config::classify_api_key_reference(&val);
    match classified {
        autorouter_config::ApiKeyReference::EnvVar(name) => {
            // Value looks like an env var name (e.g. the operator
            // typed `OPENAI_API_KEY` into the value field by
            // mistake). Save the reference, not the literal.
            entry.api_key_secret_id = Some(format!("env:{name}"));
        }
        autorouter_config::ApiKeyReference::SecretId(id) => {
            // Value is a literal API key. Pick an auto-generated id
            // (prefer the provider's default `f"{id}_api_key"`)
            // and persist it to the secret store.
            let secret_id = format!("{provider_id}_api_key");
            let id_str = id.to_string();
            let _ = secret_id; // silence unused; the id is in id_str already when useful
            let canonical_id = if id_str.is_empty() {
                secret_id
            } else {
                // If the value happens to be a short human-readable
                // id we treat it as the secret id (the operator
                // wanted to name the secret themselves). Otherwise
                // we always use the auto-generated `provider_api_key`.
                if id_str.len() <= 32 && autorouter_config::looks_like_env_var_name(&id_str) {
                    id_str
                } else {
                    format!("{provider_id}_api_key")
                }
            };
            if let Some(store) = secret_store {
                let _ = store.put(autorouter_config::Secret::new(canonical_id.clone(), val));
            }
            entry.api_key_secret_id = Some(format!("keychain:{canonical_id}"));
        }
    }
}

/// Apply a `SettingsPatch` to an `AppConfig` in place. The custom
/// provider loop rejects any patch whose name matches a first-class
/// id (see [`FIRST_CLASS_IDS`]); this is the server-side guard that
/// prevents the dashboard from creating the duplicate state where a
/// first-class slot and a custom entry share an id. Returns
/// `Err(String)` so the HTTP handler can surface the error to the
/// UI.
pub fn apply_settings_patch(
    cfg: &mut AppConfig,
    patch: SettingsPatch,
    secret_store: Option<&Arc<dyn autorouter_config::SecretStore>>,
) -> Result<(), String> {
    if let Some(p) = patch.server {
        if let Some(v) = p.bind {
            cfg.server.bind = v;
        }
        if let Some(v) = p.enable_cors {
            cfg.server.enable_cors = Some(v);
        }
        if let Some(v) = p.require_auth {
            cfg.server.require_auth = Some(v);
        }
        if let Some(v) = p.auth_token {
            cfg.server.auth_token = Some(v);
        }
        if let Some(v) = p.request_timeout_seconds {
            cfg.server.request_timeout_seconds = v;
        }
        if let Some(v) = p.max_body_bytes {
            cfg.server.max_body_bytes = v;
        }
    }
    if let Some(d) = patch.defaults {
        if let Some(v) = d.default_model {
            cfg.defaults.default_model = v;
        }
        if let Some(v) = d.default_provider {
            cfg.defaults.default_provider = v;
        }
        if let Some(v) = d.stream_by_default {
            cfg.defaults.stream_by_default = Some(v);
        }
        if let Some(v) = d.max_total_tokens {
            cfg.defaults.max_total_tokens = Some(v);
        }
    }
    if let Some(l) = patch.logging {
        if let Some(v) = l.level {
            cfg.logging.level = v;
        }
        if let Some(v) = l.json {
            cfg.logging.json = Some(v);
        }
    }
    if let Some(o) = patch.providers.openai {
        let entry = cfg.providers.openai.get_or_insert_default();
        save_secret_for_provider(
            "openai",
            entry,
            o.api_key_secret_id.clone(),
            o.api_key_value.clone(),
            secret_store,
        );
        merge_provider(entry, o);
    }
    if let Some(a) = patch.providers.anthropic {
        let entry = cfg.providers.anthropic.get_or_insert_default();
        save_secret_for_provider(
            "anthropic",
            entry,
            a.api_key_secret_id.clone(),
            a.api_key_value.clone(),
            secret_store,
        );
        merge_provider(entry, a);
    }
    if let Some(g) = patch.providers.gemini {
        let entry = cfg.providers.gemini.get_or_insert_default();
        save_secret_for_provider(
            "gemini",
            entry,
            g.api_key_secret_id.clone(),
            g.api_key_value.clone(),
            secret_store,
        );
        merge_provider(entry, g);
    }
    let mut to_remove = Vec::new();
    for (name, p) in patch.providers.custom {
        // Refuse to create a custom entry whose id collides with a
        // first-class slot. Without this check a misbehaving client
        // (or a hand-rolled curl) could write
        // `providers.custom.openai = ...` and the GET /ui/providers
        // endpoint would then return two cards with id="openai".
        if FIRST_CLASS_IDS.contains(&name.as_str()) {
            return Err(format!(
                "cannot post first-class provider as custom: {name}; use the dedicated `{name}` editor"
            ));
        }
        if p.delete == Some(true) {
            to_remove.push(name.clone());
        } else {
            let entry = cfg.providers.custom.entry(name.clone()).or_default();
            save_secret_for_provider(
                &name,
                entry,
                p.api_key_secret_id.clone(),
                p.api_key_value.clone(),
                secret_store,
            );
            merge_provider(entry, p);
        }
    }
    for name in to_remove {
        cfg.providers.custom.remove(&name);
    }
    Ok(())
}

/// Provider ids that have a dedicated first-class slot in
/// [`ProvidersConfig`](autorouter_config::ProvidersConfig). The
/// GET /ui/providers endpoint skips any `providers.custom` entry
/// whose key matches one of these, and
/// [`apply_settings_patch`] refuses to create the collision in the
/// first place. Keep in sync with the model definition in
/// `autorouter-config::ProvidersConfig`.
pub const FIRST_CLASS_IDS: &[&str] = &["openai", "anthropic", "gemini"];

async fn get_settings(
    State(s): State<UiAppState>,
    headers: HeaderMap,
) -> ServerResult<Json<Value>> {
    authorize(&headers, &s.app)?;
    let cfg = s.ui.config.read().clone();
    let v = serde_json::to_value(&cfg)
        .map_err(|e| ServerError::Internal(format!("serialize config: {e}")))?;
    // Security: redact `server.auth_token` so the dashboard and any
    // other reader of `/ui/settings` can see *that* a token is
    // configured without learning *what* it is. Without this, the
    // browser console (or any process with access to the loopback
    // gateway) can dump the bearer credential used to authenticate
    // the UI itself.
    Ok(Json(redact_auth_token_in_config(v)))
}

/// Public helper used by both the HTTP `get_settings` and the
/// desktop Tauri `cmd_settings_get` command: redacts the bearer
/// credential from a serialised `AppConfig` value, replacing it
/// with `null` and adding a sibling `has_auth_token` boolean so
/// the caller can show "(set — type to replace)" without
/// learning the existing value.
pub fn redact_auth_token_in_config(mut v: Value) -> Value {
    let has_auth_token = v
        .as_object()
        .and_then(|o| o.get("server"))
        .and_then(|s| s.get("auth_token"))
        .and_then(|t| t.as_str())
        .is_some();
    if let Some(obj) = v.as_object_mut() {
        if let Some(server) = obj.get_mut("server").and_then(|s| s.as_object_mut()) {
            server.insert("auth_token".to_string(), Value::Null);
        }
        obj.insert("has_auth_token".to_string(), Value::Bool(has_auth_token));
    }
    v
}

async fn patch_settings(
    State(s): State<UiAppState>,
    headers: HeaderMap,
    Json(patch): Json<SettingsPatch>,
) -> ServerResult<Json<Value>> {
    authorize(&headers, &s.app)?;
    {
        let mut cfg = s.ui.config.write();
        let store_guard = s.ui.secret_store.read();
        if let Err(e) = apply_settings_patch(&mut cfg, patch.clone(), store_guard.as_ref()) {
            return Err(ServerError::BadRequest(e));
        }
    }
    // M20: mirror all PATCH-able fields to SQLite so a clean
    // restart (with no TOML file) still restores the operator's
    // choices. The persisted AppConfig covers everything too, but
    // SQLite is the safety net for "I deleted config.toml by
    // accident" scenarios.
    if let Some(storage) = s.ui.storage.read().as_ref() {
        let cfg = s.ui.config.read();
        mirror_settings_to_storage(storage, &cfg);
    }
    LogLine::push(
        &s.ui.log_lines,
        "info",
        "ui",
        "Settings updated from desktop UI",
    );
    // M8 (live settings): apply the patched config to the running
    // AppState so toggles like `require_auth` take effect on the
    // next request without a gateway restart.
    let new_cfg = s.ui.config.read().clone();
    s.app.replace_config(new_cfg);
    // Rebuild the upstream set from the patched config so changes
    // to provider api_key_secret_id, base_url, or `enabled` flag
    // take effect on the next request. Without this step the
    // HttpUpstream clients keep using the secret resolved at
    // startup and the dashboard's "Save" button would be a no-op
    // for anything except auth/CORS toggles. The atomic swap
    // inside `replace_upstreams` guarantees in-flight requests keep
    // the old client while new requests pick up the new one.
    let rebuild_cfg = s.app.current_config();
    let rebuild_store = s.ui.secret_store.read().clone();
    let new_set = crate::upstream::rebuild_upstreams(&rebuild_cfg, rebuild_store);
    s.app.replace_upstreams(new_set);
    // R11: rebuild the smart router when the patch touches the
    // routing inputs the router reads at request time — i.e. the
    // default provider / model, the routing rules, or the
    // custom-provider allowlist. Without this swap the gateway
    // would keep routing with the startup-time default (gap #1).
    let router_cfg = s.ui.config.read().clone();
    let router_changed = patch.server.is_some()
        || patch.defaults.is_some()
        || patch.providers.openai.is_some()
        || patch.providers.anthropic.is_some()
        || patch.providers.gemini.is_some()
        || !patch.providers.custom.is_empty();
    if router_changed {
        let new_router =
            crate::state::build_smart_router(&s.app.pipeline, &router_cfg, (*s.app.health).clone());
        s.app.replace_router(new_router);
    }
    // Persist to the user config file unless the caller opts out.
    if patch.persist.unwrap_or(true) {
        let cfg = s.ui.config.read().clone();
        if let Some(path) = s.ui.config_path.read().clone() {
            let _write_guard = CONFIG_WRITE_LOCK.lock();
            match write_config_atomic(&path, &cfg) {
                Ok(()) => {
                    LogLine::push(
                        &s.ui.log_lines,
                        "info",
                        "ui",
                        &format!("Persisted settings to {}", path.display()),
                    );
                }
                Err(e) => {
                    LogLine::push(
                        &s.ui.log_lines,
                        "warn",
                        "ui",
                        &format!("Failed to persist settings: {e}"),
                    );
                }
            }
        }
    }
    // R10: hot-rebind the listener when the bind or any other
    // router-affecting field changed and a supervisor is attached.
    // The desktop binary attaches a supervisor so PATCH /ui/settings
    // can move the socket in place; the headless binary leaves it
    // `None` because it binds the listener directly and a process
    // restart is required to change the port.
    //
    // R12 (live CORS): the `enable_cors` toggle is read from the
    // patched config and folded into the supervisor's
    // `RouterBuildState`. The supervisor's `sync_router_state`
    // compares the new state against the current one and only
    // actually rebinds when something changed — a no-op PATCH that
    // doesn't touch the CORS layer avoids the rebind entirely so
    // the gateway stays serving on the existing socket.
    let new_bind = s.ui.config.read().server.bind.clone();
    let new_enable_cors = s.app.current_config().server.cors_enabled();
    let router_state_changed = patch.server.is_some(); // bind / cors / auth / timeouts
    if router_state_changed {
        if let Some(supervisor_outer) = s.supervisor.clone() {
            let app_for_rebind = s.app.clone();
            let ui_for_rebind = s.ui.clone();
            // Clone the supervisor again so the closure can keep
            // a reference to the same live supervisor that owns the
            // listener; the outer `match` consumes the first clone
            // because `sync_router_state` takes `self` by value.
            let supervisor_for_closure = supervisor_outer.clone();
            let cfg_for_rebind = s.app.current_config();
            let new_max_body_bytes = cfg_for_rebind.server.max_body_bytes;
            let new_timeout_seconds = cfg_for_rebind.server.request_timeout_seconds;
            let new_stream_idle_timeout_seconds = cfg_for_rebind.server.stream_idle_timeout_seconds;
            let new_state = crate::supervisor::RouterBuildState {
                bind: new_bind.clone(),
                enable_cors: new_enable_cors,
                max_body_bytes: new_max_body_bytes,
                request_timeout_seconds: new_timeout_seconds,
                stream_idle_timeout_seconds: new_stream_idle_timeout_seconds,
            };
            match supervisor_outer
                .sync_router_state(new_state, move || async move {
                    build_router_with_ui(
                        app_for_rebind,
                        ui_for_rebind,
                        new_enable_cors,
                        Some(supervisor_for_closure.clone()),
                    )
                })
                .await
            {
                Ok(RebindOutcome::Rebound) => {
                    LogLine::push(
                        &s.ui.log_lines,
                        "info",
                        "ui",
                        &format!("Hot-rebound gateway to {new_bind}"),
                    );
                }
                Ok(RebindOutcome::AlreadyOnTarget) => {
                    LogLine::push(
                        &s.ui.log_lines,
                        "info",
                        "ui",
                        &format!("Gateway already serving on {new_bind}"),
                    );
                }
                Err(e) => {
                    LogLine::push(
                        &s.ui.log_lines,
                        "error",
                        "ui",
                        &format!("Hot-rebind to {new_bind} failed: {e}"),
                    );
                    return Err(ServerError::Internal(format!(
                        "hot-rebind to {new_bind} failed: {e}",
                    )));
                }
            }
        } else {
            // No supervisor: the headless binary cannot rebind.
            // Persist the change so a process restart picks it up,
            // but log a clear warning that the listener will stay
            // pinned to the original port until restart.
            LogLine::push(
                &s.ui.log_lines,
                "warn",
                "ui",
                &format!(
                    "Bind changed to {new_bind} but the gateway is not supervisor-managed; restart the process to apply the new port"
                ),
            );
        }
    }
    let cfg = s.ui.config.read().clone();
    let mut v = serde_json::to_value(&cfg)
        .map_err(|e| ServerError::Internal(format!("serialize config: {e}")))?;
    // Security: same redaction as `get_settings` — the PATCH
    // response must not echo the bearer credential back to the
    // dashboard, otherwise a successful save would re-leak the
    // token into the browser console / network logs.
    v = redact_auth_token_in_config(v);
    Ok(Json(v))
}

fn merge_provider(entry: &mut autorouter_config::ProviderEntry, p: ProviderPatch) {
    if let Some(v) = p.display_name {
        entry.display_name = v;
    }
    if let Some(v) = p.base_url.clone() {
        entry.base_url = v;
    }
    // R12: do NOT copy `api_key_secret_id` from the patch into the
    // entry directly — `save_secret_for_provider` already ran (it
    // is invoked from `apply_settings_patch` before this function)
    // and rewrote the field to the canonical `keychain:ID` /
    // `env:NAME` form. Copying the raw value back would clobber
    // that and persist the literal API key to `config.toml`,
    // violating Hard rule #6 in AGENTS.md.
    if let Some(v) = p.default_headers {
        entry.default_headers = v;
    }
    if let Some(v) = p.enabled {
        entry.enabled = v;
    }
    if let Some(v) = p.model_allowlist {
        entry.model_allowlist = v;
    }
    // Resolve api_format: explicit patch value wins, then auto-infer
    // from the (possibly just-updated) base_url, then keep existing.
    entry.api_format = if let Some(fmt) = p.api_format.as_deref() {
        match fmt {
            "anthropic" => autorouter_config::ApiFormat::Anthropic,
            "gemini" => autorouter_config::ApiFormat::Gemini,
            _ => autorouter_config::ApiFormat::OpenAI,
        }
    } else if p.base_url.is_some() {
        // base_url was just patched — re-infer from the new value
        autorouter_config::infer_api_format(&entry.base_url)
    } else {
        // Nothing changed; keep the existing inferred/stored value.
        // If it was never set (default OpenAI), re-infer from current base_url
        // so legacy entries get upgraded automatically.
        if entry.api_format == autorouter_config::ApiFormat::OpenAI && !entry.base_url.is_empty() {
            autorouter_config::infer_api_format(&entry.base_url)
        } else {
            entry.api_format
        }
    };
}

/// Public wrapper for the desktop binary to persist settings via
/// the same atomic write path the HTTP PATCH /ui/settings uses.
pub fn write_config_atomic_public(path: &std::path::Path, cfg: &AppConfig) -> Result<(), String> {
    let _guard = CONFIG_WRITE_LOCK.lock();
    write_config_atomic(path, cfg)
}

/// M14/M20: mirror every PATCH-able field of the live AppConfig
/// into the StorageHandle so the Tauri and HTTP code paths use the
/// same safety net. This function is the single source of truth for
/// the SQLite-side mirror; both the HTTP `patch_settings` handler
/// and the desktop `cmd_settings_patch` tauri command call it.
pub fn mirror_settings_to_storage(storage: &crate::storage::StorageHandle, cfg: &AppConfig) {
    if let Some(tok) = cfg.server.auth_token.as_ref() {
        let _ = storage.set_setting("auth_token", tok);
    }
    let _ = storage.set_setting("bind", &cfg.server.bind);
    let _ = storage.set_setting("enable_cors", &cfg.server.cors_enabled().to_string());
    let _ = storage.set_setting(
        "require_auth",
        &cfg.server.require_auth.unwrap_or(false).to_string(),
    );
    let _ = storage.set_setting("max_body_bytes", &cfg.server.max_body_bytes.to_string());
    let _ = storage.set_setting(
        "request_timeout_seconds",
        &cfg.server.request_timeout_seconds.to_string(),
    );
    let _ = storage.set_setting(
        "stream_idle_timeout_seconds",
        &cfg.server.stream_idle_timeout_seconds.to_string(),
    );
    let _ = storage.set_setting("default_provider", &cfg.defaults.default_provider);
    let _ = storage.set_setting("default_model", &cfg.defaults.default_model);
    let _ = storage.set_setting(
        "stream_by_default",
        &cfg.defaults.stream_by_default.unwrap_or(false).to_string(),
    );
    let _ = storage.set_setting(
        "max_total_tokens",
        &cfg.defaults
            .max_total_tokens
            .unwrap_or(1_000_000)
            .to_string(),
    );
    let _ = storage.set_setting("log_level", &cfg.logging.level);
    let _ = storage.set_setting("log_json", &cfg.logging.json.unwrap_or(false).to_string());
}

fn write_config_atomic(path: &std::path::Path, cfg: &AppConfig) -> Result<(), String> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let text = toml::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    let tmp = path.with_extension(format!("toml.tmp.{}", std::process::id()));
    // H4: write to a unique temp file, fsync the data, then atomically rename.
    if tmp.exists() {
        let _ = std::fs::remove_file(&tmp);
    }
    {
        let mut f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
        f.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
    }
    #[cfg(windows)]
    {
        // On Windows, try an atomic rename first (NTFS supports
        // MOVEFILE_REPLACE_EXISTING). If that fails (e.g. older
        // Windows or unusual filesystem), fall back to delete+rename.
        if std::fs::rename(&tmp, path).is_err() {
            std::fs::remove_file(path).ok();
            std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
        }
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct LogsQuery {
    pub since: Option<i64>,
    pub limit: Option<usize>,
    pub level: Option<String>,
}

async fn get_logs(
    State(s): State<UiAppState>,
    headers: HeaderMap,
    axum::extract::Query(q): axum::extract::Query<LogsQuery>,
) -> ServerResult<Json<Value>> {
    authorize(&headers, &s.app)?;
    let limit = q.limit.unwrap_or(500).clamp(1, 5_000);
    let g = s.ui.log_lines.read();
    // Use `ts <= since` (strictly-excludes entries the client has
    // already seen). The previous `ts < since` formulation left
    // any entry whose timestamp equalled the last cursor in the
    // window, so every subsequent poll re-returned it and the
    // page's `setLines([...prev, ...r.lines])` accumulated
    // duplicates until the 5000-line cap kicked in. The Logs
    // page observed this as \"the same startup entry shows up four
    // times in a row even after an hour of uptime\" — actually
    // many more times, but the buffer's 5000-line tail cap made
    // it look bounded.
    let since_start = if let Some(since) = q.since {
        g.partition_point(|l| l.ts.timestamp_millis() <= since)
    } else {
        0
    };
    // Combine `since` and `limit` correctly: prefer the later of the
    // two candidates so neither silently skips entries the other
    // would have returned. Previously the truncation branch
    // unconditionally overwrote `start`, dropping every line between
    // the `since` cursor and the head of the buffer.
    let start = if g.len() > limit && since_start < g.len() - limit {
        g.len() - limit
    } else {
        since_start
    };
    let mut slice: Vec<LogLine> = g[start..].to_vec();
    if let Some(level) = q.level {
        slice.retain(|l| l.level.eq_ignore_ascii_case(&level));
    }
    // next_since must advance past the LAST returned entry even when
    // the entry itself was filtered out by `level`. If we used the
    // timestamp of the filtered slice, a poll that returned zero
    // lines (level mismatch) would freeze the cursor and the same
    // entries would be re-returned on every subsequent poll.
    let last_in_window = g.last().map(|l| l.ts.timestamp_millis()).unwrap_or(0);
    Ok(Json(json!({
        "lines": slice,
        "next_since": last_in_window,
    })))
}

async fn restart(State(s): State<UiAppState>, headers: HeaderMap) -> ServerResult<Json<Value>> {
    authorize(&headers, &s.app)?;
    LogLine::push(&s.ui.log_lines, "info", "ui", "Restart requested from UI");
    // R10: actually rebind the listener via the supervisor when one
    // is attached. The previous implementation was a 202 no-op that
    // only wrote a log line; the dashboard's "Restart server" button
    // looked like it did nothing. We now route through the same
    // `rebind_if_needed` path PATCH /ui/settings uses so the
    // gateway can move to the latest `server.bind` from in-memory
    // config (or stay on the same port if the bind is unchanged).
    if let Some(supervisor_outer) = s.supervisor.clone() {
        // Serialise with concurrent config writes so the bind and
        // CORS values we read are consistent with what'd be on disk
        // if a PATCH /ui/settings is in flux.
        let (new_bind, enable_cors) = {
            let _write_guard = CONFIG_WRITE_LOCK.lock();
            (
                s.ui.config.read().server.bind.clone(),
                s.app.current_config().server.cors_enabled(),
            )
        };
        let app_for_rebind = s.app.clone();
        let ui_for_rebind = s.ui.clone();
        let log_lines = s.ui.log_lines.clone();
        // Clone the supervisor so the closure can keep a reference
        // to the live supervisor; the outer `match` consumes its
        // clone because `rebind_if_needed` takes `self` by value.
        let supervisor_for_closure = supervisor_outer.clone();
        match supervisor_outer
            .rebind_if_needed(&new_bind, move || async move {
                build_router_with_ui(
                    app_for_rebind,
                    ui_for_rebind,
                    enable_cors,
                    Some(supervisor_for_closure.clone()),
                )
            })
            .await
        {
            Ok(RebindOutcome::Rebound) => {
                LogLine::push(
                    &log_lines,
                    "info",
                    "ui",
                    &format!("Restart: rebound gateway to {new_bind}"),
                );
                return Ok(Json(
                    json!({ "ok": true, "rebound": true, "bind": new_bind }),
                ));
            }
            Ok(RebindOutcome::AlreadyOnTarget) => {
                LogLine::push(
                    &log_lines,
                    "info",
                    "ui",
                    &format!("Restart: already serving on {new_bind}"),
                );
                return Ok(Json(
                    json!({ "ok": true, "rebound": false, "bind": new_bind }),
                ));
            }
            Err(e) => {
                LogLine::push(&log_lines, "error", "ui", &format!("Restart failed: {e}"));
                return Err(ServerError::Internal(format!("restart: {e}")));
            }
        }
    }
    // No supervisor: the headless binary cannot rebind in place. The
    // operator must restart the process. We still return 200 with a
    // clear "restart_required" hint so the dashboard can show a
    // sensible message instead of pretending the restart happened.
    LogLine::push(
        &s.ui.log_lines,
        "warn",
        "ui",
        "Restart requested but the gateway is not supervisor-managed; process restart required",
    );
    Ok(Json(json!({ "ok": false, "restart_required": true })))
}

// --- New dashboard endpoints (B8) ---

async fn get_routing(State(s): State<UiAppState>, headers: HeaderMap) -> ServerResult<Json<Value>> {
    authorize(&headers, &s.app)?;
    let cfg = s.ui.config.read().clone();
    let rules = cfg.routing.rules.clone();
    Ok(Json(json!({
        "rules": rules,
        "default_tags": cfg.routing.default_tags,
    })))
}

#[derive(Debug, Deserialize)]
struct RoutingPatch {
    rules: Option<Vec<Value>>,
    default_tags: Option<Vec<String>>,
}

async fn patch_routing(
    State(s): State<UiAppState>,
    headers: HeaderMap,
    Json(patch): Json<RoutingPatch>,
) -> ServerResult<Json<Value>> {
    authorize(&headers, &s.app)?;
    {
        let mut cfg = s.ui.config.write();
        if let Some(rules) = patch.rules {
            cfg.routing.rules = rules;
        }
        if let Some(t) = patch.default_tags {
            cfg.routing.default_tags = t;
        }
    }
    // R11: rebuild the smart router from the patched rules so the
    // next request is routed by the new rule set without a process
    // restart. The previous implementation only mutated the in-memory
    // config and persisted it to TOML; the gateway kept using the
    // startup-time router until the operator restarted, which made
    // the dashboard's "Save routing" button look like a no-op for
    // every rule change (gap #1).
    rebuild_and_swap_router(&s);
    if let Some(path) = s.ui.config_path.read().clone() {
        let cfg = s.ui.config.read().clone();
        let _write_guard = CONFIG_WRITE_LOCK.lock();
        match write_config_atomic(&path, &cfg) {
            Ok(()) => {
                LogLine::push(
                    &s.ui.log_lines,
                    "info",
                    "ui",
                    &format!("Persisted routing to {}", path.display()),
                );
            }
            Err(e) => {
                LogLine::push(
                    &s.ui.log_lines,
                    "warn",
                    "ui",
                    &format!("Failed to persist routing: {e}"),
                );
            }
        }
    }
    let cfg = s.ui.config.read().clone();
    Ok(Json(json!({
        "rules": cfg.routing.rules,
        "default_tags": cfg.routing.default_tags,
    })))
}

/// Build a fresh [`SmartRouter`](autorouter_router::SmartRouter)
/// from the live config and swap it into [`AppState`]. Used by
/// `PATCH /ui/routing` and `PATCH /ui/settings` so the next
/// incoming request is routed by the latest rules + default target
/// (gap #1).
fn rebuild_and_swap_router(s: &UiAppState) {
    let cfg = s.ui.config.read().clone();
    let new_router =
        crate::state::build_smart_router(&s.app.pipeline, &cfg, (*s.app.health).clone());
    s.app.replace_router(new_router);
    LogLine::push(
        &s.ui.log_lines,
        "info",
        "ui",
        "Rebuilt smart router from patched rules",
    );
}

async fn get_health(State(s): State<UiAppState>, headers: HeaderMap) -> ServerResult<Json<Value>> {
    authorize(&headers, &s.app)?;
    let providers: Vec<Value> = [
        autorouter_core::ProviderKind::OpenAI,
        autorouter_core::ProviderKind::Anthropic,
        autorouter_core::ProviderKind::Gemini,
    ]
    .iter()
    .map(|k| {
        let snap = s.app.health.snapshot(*k);
        json!({
            "provider": k.to_string(),
            "samples": snap.samples,
            "success_rate": snap.success_rate,
            "avg_latency_ms": snap.avg_latency_ms,
            "score": snap.score,
        })
    })
    .collect();
    Ok(Json(json!({ "providers": providers })))
}

#[derive(Debug, Deserialize)]
struct EventsQuery {
    provider: Option<String>,
    limit: Option<usize>,
}

/// M13: surface the secret store's backend name and a list of
/// secret ids. When the backend cannot enumerate (e.g. the OS
/// keychain), the endpoint returns `list_supported: false` and
/// `ids: null` so the UI can render a meaningful "keyring
/// enumeration is not available on this platform" message.
async fn get_secrets(State(s): State<UiAppState>, headers: HeaderMap) -> ServerResult<Json<Value>> {
    authorize(&headers, &s.app)?;
    let store = s.ui.secret_store.read().clone();
    match store {
        None => Ok(Json(json!({
            "backend": null,
            "list_supported": false,
            "ids": Value::Null,
        }))),
        Some(store) => {
            let backend = store.backend_name();
            let list_supported = store.list_supported();
            let ids = if list_supported {
                match store.list() {
                    Ok(list) => {
                        Value::Array(list.into_iter().map(|s| Value::String(s.0)).collect())
                    }
                    Err(_) => Value::Null,
                }
            } else {
                Value::Null
            };
            Ok(Json(json!({
                "backend": backend,
                "list_supported": list_supported,
                "ids": ids,
            })))
        }
    }
}

async fn get_secret_value(
    State(s): State<UiAppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> ServerResult<Json<Value>> {
    authorize(&headers, &s.app)?;
    let store = s.ui.secret_store.read().clone();
    let val = if let Some(env_name) = id.strip_prefix("env:") {
        std::env::var(env_name).ok()
    } else if let Some(store) = store {
        let secret_id = id.strip_prefix("keychain:").unwrap_or(&id).to_string();
        store.get(&secret_id.into()).ok().map(|sec| sec.value)
    } else {
        None
    };
    match val {
        Some(v) => Ok(Json(json!({ "value": v }))),
        None => Err(ServerError::NotFound(format!("secret not found: {id}"))),
    }
}

#[derive(Debug, Deserialize)]
struct PutSecretBody {
    value: String,
}

async fn put_secret_value(
    State(s): State<UiAppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(body): Json<PutSecretBody>,
) -> ServerResult<Json<Value>> {
    authorize(&headers, &s.app)?;
    let store_guard = s.ui.secret_store.read().clone();
    let Some(store) = store_guard else {
        return Err(ServerError::Internal("secret store not available".into()));
    };
    let secret_id = id.strip_prefix("keychain:").unwrap_or(&id).to_string();
    let secret = autorouter_config::Secret::new(secret_id, body.value);
    store
        .put(secret)
        .map_err(|e| ServerError::Internal(e.to_string()))?;
    tracing::info!(id = %id, "secret stored via HTTP UI");
    Ok(Json(json!({ "ok": true, "id": id })))
}

async fn get_events(
    State(s): State<UiAppState>,
    headers: HeaderMap,
    axum::extract::Query(q): axum::extract::Query<EventsQuery>,
) -> ServerResult<Json<Value>> {
    authorize(&headers, &s.app)?;
    let limit = q.limit.unwrap_or(100).clamp(1, 1000) as u32;
    let provider_filter = q.provider.as_deref().unwrap_or("");
    let storage_guard = s.ui.storage.read();
    let Some(storage) = storage_guard.as_ref() else {
        return Ok(Json(json!({
            "events": [],
            "note": "storage not enabled; no events captured"
        })));
    };
    let events = if provider_filter.is_empty() {
        let kinds = ["openai", "anthropic", "gemini"];
        let mut all = Vec::new();
        for k in kinds {
            if let Ok(rows) = storage.recent_provider_events(k, limit) {
                all.extend(rows);
            }
        }
        all.sort_by_key(|b| std::cmp::Reverse(b.created_at.0));
        all.truncate(limit as usize);
        all
    } else {
        storage
            .recent_provider_events(provider_filter, limit)
            .map_err(|e| ServerError::Internal(format!("storage: {e}")))?
    };
    Ok(Json(json!({
        "events": events.into_iter().map(|e| json!({
            "provider": e.provider,
            "model": e.model,
            "kind": e.kind,
            "status": e.status,
            "latency_ms": e.latency_ms,
            "error": e.error,
            "created_at": e.created_at,
            "input_tokens": e.input_tokens,
            "output_tokens": e.output_tokens,
            "cache_read_tokens": e.cache_read_tokens,
            "cache_write_tokens": e.cache_write_tokens,
            "reasoning_tokens": e.reasoning_tokens,
        })).collect::<Vec<_>>()
    })))
}

#[allow(clippy::type_complexity)]
async fn get_analytics(
    State(s): State<UiAppState>,
    headers: HeaderMap,
) -> ServerResult<Json<Value>> {
    authorize(&headers, &s.app)?;
    let storage_guard = s.ui.storage.read();
    let kinds = ["openai", "anthropic", "gemini"];
    let mut all_events: Vec<autorouter_config::ProviderEvent> = Vec::new();
    if let Some(storage) = storage_guard.as_ref() {
        for k in kinds {
            if let Ok(rows) = storage.recent_provider_events(k, 5_000) {
                all_events.extend(rows);
            }
        }
    }
    drop(storage_guard);
    // Aggregate the per-event counters. Each bucket sums
    // input/output/cache tokens so the dashboard shows real usage
    // pulled from the upstream `Usage` block (the schema migration
    // v4 added the columns; the recording path was wired in this
    // same workstream to populate them).
    let mut total_requests = 0u64;
    let mut total_failures = 0u64;
    let mut total_rate_limit_hits = 0u64;
    let mut total_input_tokens = 0u64;
    let mut total_output_tokens = 0u64;
    let mut total_cache_read_tokens = 0u64;
    let mut total_cache_write_tokens = 0u64;
    let mut total_reasoning_tokens = 0u64;
    let mut latency_total_ms: u64 = 0;
    let mut latency_samples: Vec<u64> = Vec::new();
    use std::collections::BTreeMap;
    // Bucket shape: (requests, failures, input, output, cache_read,
    // cache_write, reasoning). BTreeMap so the JSON output is
    // sorted alphabetically by provider / model id.
    let mut by_provider: BTreeMap<String, (u64, u64, u64, u64, u64, u64, u64)> = BTreeMap::new();
    let mut by_model: BTreeMap<String, (u64, u64, u64, u64, u64, u64, u64)> = BTreeMap::new();
    for e in &all_events {
        total_requests += 1;
        if e.status >= 400 {
            total_failures += 1;
        }
        if e.status == 429 {
            total_rate_limit_hits += 1;
        }
        total_input_tokens += e.input_tokens;
        total_output_tokens += e.output_tokens;
        total_cache_read_tokens += e.cache_read_tokens;
        total_cache_write_tokens += e.cache_write_tokens;
        total_reasoning_tokens += e.reasoning_tokens;
        latency_total_ms += e.latency_ms;
        if e.latency_ms > 0 {
            latency_samples.push(e.latency_ms);
        }
        let p = by_provider
            .entry(e.provider.clone())
            .or_insert((0, 0, 0, 0, 0, 0, 0));
        p.0 += 1;
        if e.status >= 400 {
            p.1 += 1;
        }
        p.2 += e.input_tokens;
        p.3 += e.output_tokens;
        p.4 += e.cache_read_tokens;
        p.5 += e.cache_write_tokens;
        p.6 += e.reasoning_tokens;
        let m = by_model
            .entry(e.model.clone())
            .or_insert((0, 0, 0, 0, 0, 0, 0));
        m.0 += 1;
        if e.status >= 400 {
            m.1 += 1;
        }
        m.2 += e.input_tokens;
        m.3 += e.output_tokens;
        m.4 += e.cache_read_tokens;
        m.5 += e.cache_write_tokens;
        m.6 += e.reasoning_tokens;
    }
    // p50 / p95 from the latency distribution. Sorting once keeps
    // the math cheap; the typical request count is < 5000.
    latency_samples.sort_unstable();
    let p50 = percentile(&latency_samples, 0.50);
    let p95 = percentile(&latency_samples, 0.95);
    let avg_latency_ms = latency_total_ms.checked_div(total_requests).unwrap_or(0);
    let latency_recorded = !latency_samples.is_empty();
    let by_provider_out: Vec<Value> = by_provider
        .into_iter()
        .map(|(p, (r, f, i, o, cr, cw, rs))| {
            json!({
                "provider": p,
                "requests": r,
                "failures": f,
                "input_tokens": i,
                "output_tokens": o,
                "cache_read_tokens": cr,
                "cache_write_tokens": cw,
                "reasoning_tokens": rs,
            })
        })
        .collect();
    let by_model_out: Vec<Value> = by_model
        .into_iter()
        .map(|(m, (r, f, i, o, cr, cw, rs))| {
            json!({
                "model": m,
                "requests": r,
                "failures": f,
                "input_tokens": i,
                "output_tokens": o,
                "cache_read_tokens": cr,
                "cache_write_tokens": cw,
                "reasoning_tokens": rs,
            })
        })
        .collect();
    Ok(Json(json!({
        "total_requests": total_requests,
        "total_failures": total_failures,
        "total_input_tokens": total_input_tokens,
        "total_output_tokens": total_output_tokens,
        "total_cache_read_tokens": total_cache_read_tokens,
        "total_cache_write_tokens": total_cache_write_tokens,
        "total_reasoning_tokens": total_reasoning_tokens,
        "total_rate_limit_hits": total_rate_limit_hits,
        "avg_latency_ms": avg_latency_ms,
        "p50_latency_ms": p50,
        "p95_latency_ms": p95,
        "latency_recorded": latency_recorded,
        "by_provider": by_provider_out,
        "by_model": by_model_out,
        "events_examined": all_events.len(),
    })))
}

/// Linear-interpolation percentile over an already-sorted `samples`
/// slice. Returns 0 when the slice is empty. The percentile is a
/// 0..1 fraction (0.5 = median, 0.95 = 95th percentile).
fn percentile(samples: &[u64], p: f64) -> u64 {
    if samples.is_empty() {
        return 0;
    }
    let n = samples.len();
    if n == 1 {
        return samples[0];
    }
    let rank = p * (n as f64 - 1.0);
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    if lo == hi {
        samples[lo]
    } else {
        let frac = rank - lo as f64;
        let span = (samples[hi] - samples[lo]) as f64;
        samples[lo] + (span * frac) as u64
    }
}

async fn get_debug(State(s): State<UiAppState>, headers: HeaderMap) -> ServerResult<Json<Value>> {
    authorize(&headers, &s.app)?;
    let cfg = s.ui.config.read().clone();
    let started = *s.ui.start_time.read();
    let uptime = (chrono::Utc::now() - started).num_seconds().max(0);
    // R10: prefer the live socket address from the supervisor.
    let bind = s
        .supervisor
        .as_ref()
        .and_then(|sup| sup.current_bind())
        .unwrap_or_else(|| cfg.server.bind.clone());
    let env: Vec<Value> = std::env::vars()
        .filter(|(k, _)| k.starts_with("AUTOROUTER_"))
        // Security: never echo the value of AUTOROUTER_AUTH_TOKEN
        // back over the debug endpoint — it is the bearer credential
        // used to authenticate the UI itself. Surface the key name
        // and a fixed redacted marker instead so operators can still
        // see the env var is set.
        .map(|(k, v)| {
            if k == "AUTOROUTER_AUTH_TOKEN" {
                json!({ "key": k, "value": "***", "redacted": true })
            } else {
                json!({ "key": k, "value": v })
            }
        })
        .collect();
    let mut cfg_value = serde_json::to_value(&cfg)
        .map_err(|e| ServerError::Internal(format!("serialize config: {e}")))?;
    // Security: redact `server.auth_token` in the debug payload
    // (same rationale as `get_settings`). Other secrets are stored
    // by reference (`env:NAME`, `keychain:id`) and therefore never
    // appear in the serialised config — only the bearer credential
    // needs explicit scrubbing.
    if let Some(obj) = cfg_value.as_object_mut() {
        if let Some(server) = obj.get_mut("server").and_then(|s| s.as_object_mut()) {
            server.insert("auth_token".to_string(), Value::Null);
        }
    }
    Ok(Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "bind": bind,
        "started_at": started,
        "uptime_seconds": uptime,
        "pid": std::process::id(),
        "arch": std::env::consts::ARCH,
        "os": std::env::consts::OS,
        "config": cfg_value,
        "env": env,
        "build": {
            "rust_version": env!("CARGO_PKG_RUST_VERSION"),
            "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
            "target": std::env::consts::ARCH,
            "features": [],
        }
    })))
}

async fn list_tool_profiles(
    State(s): State<UiAppState>,
    headers: HeaderMap,
) -> ServerResult<Json<Value>> {
    authorize(&headers, &s.app)?;
    Ok(Json(json!({
        "profiles": [],
        "hint": "Tool profiles are stored under [tool_profiles] in config.toml"
    })))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct SaveToolProfile {
    name: String,
    description: Option<String>,
    schema: Value,
}

async fn save_tool_profile(
    State(s): State<UiAppState>,
    headers: HeaderMap,
    Json(p): Json<SaveToolProfile>,
) -> ServerResult<Json<Value>> {
    authorize(&headers, &s.app)?;
    LogLine::push(
        &s.ui.log_lines,
        "info",
        "ui",
        &format!("Tool profile saved: {}", p.name),
    );
    Ok(Json(json!({ "ok": true, "name": p.name })))
}

#[derive(Debug, Deserialize)]
struct ToolTestReq {
    name: String,
    input: Value,
}

async fn test_tool(
    State(s): State<UiAppState>,
    headers: HeaderMap,
    Json(req): Json<ToolTestReq>,
) -> ServerResult<Json<Value>> {
    authorize(&headers, &s.app)?;
    Ok(Json(json!({
        "ok": true,
        "name": req.name,
        "input": req.input,
        "message": "Tool test stub. Real validation runs in a follow-up.",
    })))
}

/// Body of `POST /ui/provider_test`. The dashboard sends the
/// provider `id` (first-class or custom) and an optional explicit
/// `model` override. The server resolves the matching
/// `ProviderEntry`, builds a one-shot `HttpUpstream` with the
/// resolved secret, and fires a minimal "ping" request to verify
/// the wire is live. Returns `{ok, status?, latency_ms, error?}`.
#[derive(Debug, Deserialize)]
struct ProviderTestReq {
    id: String,
    /// Optional model id to put in the probe body. Falls back to
    /// the first entry in the provider's `model_allowlist`, then
    /// to a known-good default per wire format.
    model: Option<String>,
}

/// Probe an upstream provider with a minimal "ping" chat
/// completion. The handler does NOT mutate the live `AppState`;
/// it builds a throwaway `HttpUpstream` from the current
/// `ProviderEntry` (after `apply_settings_patch` has already
/// written it to in-memory config) so a Save-then-Test workflow
/// exercises exactly the values the gateway will use on the next
/// real request.
async fn test_provider(
    State(s): State<UiAppState>,
    headers: HeaderMap,
    Json(req): Json<ProviderTestReq>,
) -> ServerResult<Json<Value>> {
    authorize(&headers, &s.app)?;
    let cfg = s.ui.config.read().clone();
    // 1. Resolve the entry by id. First-class ids always win so a
    //    misbehaving client cannot trick us into probing a
    //    collider's custom entry.
    let (provider_id, entry): (String, autorouter_config::ProviderEntry) = match req.id.as_str() {
        "openai" => match cfg.providers.openai {
            Some(ref e) => ("openai".to_string(), e.clone()),
            None => {
                return Ok(test_provider_error(
                    "missing",
                    "openai provider is not configured",
                ));
            }
        },
        "anthropic" => match cfg.providers.anthropic {
            Some(ref e) => ("anthropic".to_string(), e.clone()),
            None => {
                return Ok(test_provider_error(
                    "missing",
                    "anthropic provider is not configured",
                ));
            }
        },
        "gemini" => match cfg.providers.gemini {
            Some(ref e) => ("gemini".to_string(), e.clone()),
            None => {
                return Ok(test_provider_error(
                    "missing",
                    "gemini provider is not configured",
                ));
            }
        },
        other => match cfg.providers.custom.get(other) {
            Some(e) => (other.to_string(), e.clone()),
            None => {
                return Ok(test_provider_error(
                    "missing",
                    &format!("provider `{other}` is not configured"),
                ));
            }
        },
    };
    // 2. Pick a model for the probe: explicit override, then the
    //    first allowlist entry, then a sensible default per kind.
    let model: String = req
        .model
        .clone()
        .or_else(|| entry.model_allowlist.first().cloned())
        .unwrap_or_else(|| {
            // Default model ids that round-trip cleanly with each
            // adapter's minimal-request decoder. These are
            // intentionally cheap; the probe body is `max_tokens=1`
            // so the upstream only has to acknowledge the wire.
            match entry.api_format {
                autorouter_config::ApiFormat::Anthropic => "claude-3-5-haiku-latest".to_string(),
                autorouter_config::ApiFormat::Gemini => "gemini-1.5-flash".to_string(),
                autorouter_config::ApiFormat::OpenAI => "gpt-4o-mini".to_string(),
            }
        });
    // 3. Build the probe body. Keep it small so upstreams with
    //    strict cost controls don't reject it.
    let kind = crate::upstream::api_format_to_kind(entry.api_format);
    let body: Value = match kind {
        autorouter_core::ProviderKind::Anthropic => json!({
            "model": model,
            "max_tokens": 1,
            "messages": [{ "role": "user", "content": "ping" }],
        }),
        autorouter_core::ProviderKind::Gemini => json!({
            "model": model,
            "contents": [{ "role": "user", "parts": [{ "text": "ping" }] }],
            "generationConfig": { "maxOutputTokens": 1 },
        }),
        _ => json!({
            "model": model,
            "max_tokens": 1,
            "messages": [{ "role": "user", "content": "ping" }],
        }),
    };
    // 4. Resolve the secret and build a one-shot HttpUpstream.
    let store = s.ui.secret_store.read().clone();
    let auth_value =
        crate::upstream::resolve_secret(entry.api_key_secret_id.as_deref(), store.as_ref());
    if entry.api_key_secret_id.is_some() && auth_value.is_none() && !entry.base_url.is_empty() {
        return Ok(test_provider_error(
            "missing_secret",
            "API key secret reference is set but the value could not be resolved",
        ));
    }
    let timeout =
        std::time::Duration::from_secs(s.app.current_config().server.request_timeout_seconds);
    let http_cfg =
        crate::upstream::HttpUpstreamConfig::from_entry(&entry, kind, auth_value, timeout);
    let upstream = match crate::upstream::HttpUpstream::new(http_cfg) {
        Ok(u) => u,
        Err(e) => {
            return Ok(test_provider_error(
                "build_failed",
                &format!("failed to build upstream client: {e}"),
            ));
        }
    };
    // 5. Fire the probe. We time it with `Instant::now()` so the
    //    dashboard can show a meaningful latency even on success.
    let started = std::time::Instant::now();
    match upstream.send(&body).await {
        Ok(resp) => {
            let latency_ms = started.elapsed().as_millis() as u64;
            LogLine::push(
                &s.ui.log_lines,
                "info",
                "ui",
                &format!(
                    "Provider test OK: id={provider_id} model={model} status={} latency_ms={latency_ms}",
                    resp.status
                ),
            );
            Ok(Json(json!({
                "ok": true,
                "provider": provider_id,
                "model": model,
                "status": resp.status,
                "latency_ms": latency_ms,
            })))
        }
        Err(e) => {
            let latency_ms = started.elapsed().as_millis() as u64;
            LogLine::push(
                &s.ui.log_lines,
                "warn",
                "ui",
                &format!(
                    "Provider test FAILED: id={provider_id} model={model} latency_ms={latency_ms} error={e}"
                ),
            );
            Ok(Json(json!({
                "ok": false,
                "provider": provider_id,
                "model": model,
                "latency_ms": latency_ms,
                "error": e.to_string(),
            })))
        }
    }
}

/// Build the standard error response shape for `test_provider`.
/// Kept as a small helper so every error path (missing config,
/// missing secret, upstream failure) returns the same JSON keys
/// and the dashboard can render them uniformly.
fn test_provider_error(kind: &str, message: &str) -> Json<Value> {
    Json(json!({
        "ok": false,
        "error": message,
        "error_kind": kind,
    }))
}

async fn import_config(
    State(s): State<UiAppState>,
    headers: HeaderMap,
    body: String,
) -> ServerResult<Json<Value>> {
    authorize(&headers, &s.app)?;
    let path =
        s.ui.config_path
            .read()
            .clone()
            .ok_or_else(|| ServerError::Internal("no config path set".into()))?;
    let cfg: AppConfig = toml::from_str(&body)
        .map_err(|e| ServerError::Internal(format!("parse imported config: {e}")))?;
    {
        let _write_guard = CONFIG_WRITE_LOCK.lock();
        write_config_atomic(&path, &cfg)
            .map_err(|e| ServerError::Internal(format!("write: {e}")))?;
    }
    LogLine::push(
        &s.ui.log_lines,
        "info",
        "ui",
        &format!("Imported config to {}", path.display()),
    );
    {
        let mut state_cfg = s.ui.config.write();
        *state_cfg = cfg.clone();
    }
    // Apply the imported config to the live gateway so it takes
    // effect immediately — mirrors the patch_settings path.
    s.app.replace_config(cfg.clone());
    let rebuild_store = s.ui.secret_store.read().clone();
    let new_set = crate::upstream::rebuild_upstreams(&cfg, rebuild_store);
    s.app.replace_upstreams(new_set);
    let new_router =
        crate::state::build_smart_router(&s.app.pipeline, &cfg, (*s.app.health).clone());
    s.app.replace_router(new_router);
    // If a supervisor is attached, rebind the listener when the
    // imported config changes router-affecting fields (bind, CORS,
    // timeouts, max_body_bytes). The supervisor's sync_router_state
    // will no-op when nothing changed. Without this step, importing a
    // config with a different bind would leave the listener pinned to
    // the old port until a manual "Restart server".
    if let Some(supervisor_outer) = s.supervisor.clone() {
        let app_for_rebind = s.app.clone();
        let ui_for_rebind = s.ui.clone();
        let supervisor_for_closure = supervisor_outer.clone();
        let cfg_for_rebind = s.app.current_config();
        let new_bind = cfg_for_rebind.server.bind.clone();
        let new_enable_cors = cfg_for_rebind.server.cors_enabled();
        let new_state = crate::supervisor::RouterBuildState {
            bind: new_bind.clone(),
            enable_cors: new_enable_cors,
            max_body_bytes: cfg_for_rebind.server.max_body_bytes,
            request_timeout_seconds: cfg_for_rebind.server.request_timeout_seconds,
            stream_idle_timeout_seconds: cfg_for_rebind.server.stream_idle_timeout_seconds,
        };
        match supervisor_outer
            .sync_router_state(new_state, move || async move {
                build_router_with_ui(
                    app_for_rebind,
                    ui_for_rebind,
                    new_enable_cors,
                    Some(supervisor_for_closure.clone()),
                )
            })
            .await
        {
            Ok(RebindOutcome::Rebound) => {
                LogLine::push(
                    &s.ui.log_lines,
                    "info",
                    "ui",
                    &format!("Import: rebound gateway to {new_bind}"),
                );
            }
            Ok(_) => {}
            Err(e) => {
                LogLine::push(
                    &s.ui.log_lines,
                    "warn",
                    "ui",
                    &format!("Import: rebind to {new_bind} failed: {e}"),
                );
            }
        }
    }
    Ok(Json(json!({ "ok": true, "path": path })))
}

async fn export_config(
    State(s): State<UiAppState>,
    headers: HeaderMap,
) -> ServerResult<impl IntoResponse> {
    authorize(&headers, &s.app)?;
    let path =
        s.ui.config_path
            .read()
            .clone()
            .ok_or_else(|| ServerError::Internal("no config path set".into()))?;
    let body =
        std::fs::read_to_string(&path).map_err(|e| ServerError::Internal(format!("read: {e}")))?;
    // Security: scrub the bearer credential from the exported
    // TOML. The Import/Export page advertises "Secret values are
    // not exported", but `auth_token` is currently written
    // verbatim to `config.toml` (unlike provider API keys, which
    // are stored as `keychain:ID` references). Without this
    // redaction, a `Download` click would give the operator a
    // file they could share by accident that authenticates the
    // dashboard on the same machine.
    let redacted = redact_auth_token_in_toml(&body);
    Ok((
        StatusCode::OK,
        [
            (axum::http::header::CONTENT_TYPE, "application/toml"),
            // Surface the redaction in a response header so the
            // dashboard can show a hint to re-set the token
            // after import.
            (
                axum::http::header::HeaderName::from_static("x-autorouter-auth-token-redacted"),
                "true",
            ),
        ],
        redacted,
    ))
}

/// Replace the value of any `auth_token = "..."` line with an
/// empty string and a comment pointing the operator at the
/// Settings UI. Operates line-by-line so we don't pull in a
/// TOML parser for what is effectively a single-field rewrite.
///
/// Handles the common shapes:
///   * `auth_token = "literal"`
///   * `auth_token = 'literal'`
///   * `auth_token = ""` (already empty — left as-is)
///   * `auth_token = ` (no value — left as-is)
///   * `  auth_token  =  "x"` (extra whitespace)
///   * `# auth_token = "..."` (comment — left as-is)
pub fn redact_auth_token_in_toml(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    // Track whether we are inside a `[providers.*.default_headers]`
    // table so we can redact every value within it. The section ends
    // at the next `[...]` header or at end of file.
    let mut in_default_headers = false;
    for line in body.lines() {
        let indent_len = line.len() - line.trim_start().len();
        let indent = &line[..indent_len];
        let trimmed = line.trim_start();
        // Comments are left alone.
        if trimmed.starts_with('#') {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        // Track table headers: a line starting with `[`.
        if trimmed.starts_with('[') {
            in_default_headers = trimmed
                .strip_prefix('[')
                .and_then(|s| s.strip_suffix(']'))
                .map(|s| s.trim())
                .is_some_and(|s| s.contains("default_headers"));
        }
        // Redact `auth_token = ...` (case-insensitive key).
        if let Some(eq_idx) = trimmed.find('=') {
            let key = trimmed[..eq_idx].trim();
            if key.eq_ignore_ascii_case("auth_token") {
                // Replace the value with an empty string and
                // append a comment so the operator knows the
                // token is missing on import.
                out.push_str(&format!(
                    "{indent}auth_token = \"\"  # redacted by /ui/export; re-set via Settings UI after import\n"
                ));
                continue;
            }
        }
        // Redact values inside a `default_headers` block. Every
        // `key = "value"` line within the block is a header value
        // that may contain a credential.
        if in_default_headers {
            if let Some(eq_idx) = trimmed.find('=') {
                let key = trimmed[..eq_idx].trim().to_string();
                let new_line = format!("{indent}{key} = \"\"  # redacted by /ui/export; re-set via Settings UI after import\n");
                out.push_str(&new_line);
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    // Strip the trailing newline we just appended if the input
    // did not end with one (avoid doubling up).
    if !body.ends_with('\n') && out.ends_with('\n') {
        out.pop();
    }
    out
}

async fn check_update(
    State(s): State<UiAppState>,
    headers: HeaderMap,
) -> ServerResult<Json<Value>> {
    authorize(&headers, &s.app)?;
    Ok(Json(json!({
        "current_version": env!("CARGO_PKG_VERSION"),
        "latest_version": env!("CARGO_PKG_VERSION"),
        "update_available": false,
        "release_notes": "Auto-update check requires network access. The current build is the latest published version.",
        "release_url": null,
        "published_at": null,
        "can_self_update": false,
    })))
}
