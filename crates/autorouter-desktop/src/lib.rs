#![deny(unused_crate_dependencies)]
//! Tauri 2 shell hosting the AutoRouter gateway and the dashboard UI.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use tokio as _;

use serde_json::Value;
use tauri::{
    image::Image,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, RunEvent, WindowEvent,
};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use tauri_plugin_opener::OpenerExt;

use autorouter_config::{ConfigLoader, ProjectPaths};
use autorouter_observability::{init_logging, install_log_sink, LoggingConfig};
use autorouter_server::{
    ui::UiState, AppState, GatewaySupervisor, LogBridge, RebindOutcome, TranslationPipeline,
    UpstreamClient,
};
use autorouter_translate::{
    AnthropicAdapter, GeminiAdapter, OpenAiChatAdapter, OpenAiResponsesAdapter,
};

/// The global keyboard shortcut that opens the provider/model
/// switcher overlay. Exposed as a constant so the frontend
/// status bar / settings page can advertise it without
/// duplicating the chord.
///
/// Tauri parses chord strings with the `global-hotkey` crate,
/// e.g. `"CommandOrControl+Shift+P"`. We register exactly one
/// shortcut — see AGENTS.md "DO NOT" rules in the prompt.
const SWITCHER_SHORTCUT: &str = "CommandOrControl+Shift+P";

/// Tauri event emitted when the user presses the switcher
/// shortcut. The frontend `Switcher` overlay listens for this
/// and pops itself on top of every other surface (above the
/// Tauri main window, above the tray, above other apps).
const SWITCHER_EVENT: &str = "show-switcher";

/// Bundle of state shared across the Tauri runtime.
pub struct DesktopState {
    pub config: Arc<parking_lot::RwLock<autorouter_config::AppConfig>>,
    pub ui_state: UiState,
    pub app_state: AppState,
    /// The address the gateway is currently bound to. Mirrors the
    /// supervisor's live socket, but is kept in a small lock so
    /// synchronous Tauri commands can read it without awaiting.
    pub bind: Arc<parking_lot::RwLock<String>>,
    /// The supervisor that owns the running `axum::serve` task and
    /// the `TcpListener` behind it. Cloned cheaply via `Arc`.
    pub supervisor: GatewaySupervisor,
    /// Legacy field kept for backwards compatibility with
    /// `Onboarding` and any external code that read the value at
    /// startup; populated by the same path that updates `bind`.
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// LogBridge handle so the background drain task can be
    /// stopped on shutdown. Kept alive for the process lifetime.
    pub log_bridge: Option<autorouter_server::LogBridge>,
}

#[tauri::command]
fn get_status(state: tauri::State<DesktopState>) -> Result<Value, String> {
    let ui = &state.ui_state;
    let cfg = ui.config.read().clone();
    let started = *ui.start_time.read();
    let uptime = (chrono::Utc::now() - started).num_seconds().max(0);
    // Prefer the supervisor's live address so the dashboard
    // reflects the actual socket, not the value captured at
    // startup. Falls back to the cached `bind` lock while the
    // supervisor is briefly between listeners during a rebind.
    let bind = state
        .supervisor
        .current_bind()
        .unwrap_or_else(|| state.bind.read().clone());
    Ok(serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "bind": bind,
        "started_at": started,
        "uptime_seconds": uptime,
        "log_lines": ui.log_lines.read().len(),
        "session_count": state.app_state.sessions.list().len(),
        "providers": {
            "openai": cfg.providers.openai.is_some(),
            "anthropic": cfg.providers.anthropic.is_some(),
            "gemini": cfg.providers.gemini.is_some(),
        }
    }))
}

#[tauri::command]
fn open_external(app: AppHandle, url: String) -> Result<(), String> {
    app.opener()
        .open_url(url.as_str(), None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn reveal_data_dir(app: AppHandle) -> Result<String, String> {
    let paths =
        ProjectPaths::resolve().ok_or_else(|| "Could not resolve project paths".to_string())?;
    let path = paths.data_dir.to_string_lossy().to_string();
    app.opener()
        .open_path(path.as_str(), None::<&str>)
        .map_err(|e| format!("Failed to open directory: {e}"))?;
    Ok(path)
}

/// H10: storage handle cached at gateway build so the quit
/// handler can run the shutdown backup before the process exits.
static SHUTDOWN_HANDLES: OnceLock<ShutdownHandles> = OnceLock::new();

/// Ensures the shutdown backup runs at most once per process. The
/// tray "quit" handler and the `quit_app` command both perform the
/// backup and then call `app.exit(0)`, which re-fires
/// `RunEvent::ExitRequested`. Without this guard the (potentially
/// slow) SQLite backup runs twice and doubles shutdown latency.
static SHUTDOWN_BACKUP_DONE: AtomicBool = AtomicBool::new(false);

struct ShutdownHandles {
    storage: Option<Arc<autorouter_server::StorageHandle>>,
    /// Directory holding timestamped backups
    /// (`autorouter.db.<UTC stamp>`). Pruned to `backup_keep` on
    /// every clean shutdown.
    backup_dir: std::path::PathBuf,
    /// Live database filename (not full path) used as the prune
    /// prefix so only matching timestamped files are removed.
    database_file: String,
    backup_keep: u32,
    backup_on_shutdown: bool,
}

fn perform_shutdown_backup(handles: &ShutdownHandles) {
    if !handles.backup_on_shutdown {
        return;
    }
    if SHUTDOWN_BACKUP_DONE.swap(true, Ordering::SeqCst) {
        return;
    }
    if let Some(storage) = handles.storage.as_ref() {
        if let Err(e) = std::fs::create_dir_all(&handles.backup_dir) {
            tracing::warn!(error = %e, "failed to create backup dir");
            return;
        }
        let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
        let backup_path = handles
            .backup_dir
            .join(format!("{}.{stamp}", handles.database_file));
        if let Err(e) = storage.shutdown(Some(&backup_path)) {
            tracing::warn!(error = %e, "shutdown backup failed");
        }
        // Keep only the newest `backup_keep` timestamped files so
        // the backups directory cannot grow without bound.
        match autorouter_observability::prune_timestamped_backups(
            &handles.backup_dir,
            &handles.database_file,
            handles.backup_keep,
        ) {
            Ok(n) if n > 0 => {
                tracing::info!(pruned = n, keep = handles.backup_keep, "pruned old backups");
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(error = %e, "backup prune failed");
            }
        }
    }
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    // H10: perform the configured backup BEFORE the process exits.
    if let Some(handles) = SHUTDOWN_HANDLES.get() {
        perform_shutdown_backup(handles);
    }
    // Stop the LogBridge drain task and gracefully stop the gateway
    // before the process exits. Without stopping the bridge here, any
    // log entries still in the sink at quit time are lost (M18).
    // Without stopping the gateway, in-flight clients see a TCP reset.
    if let Some(state) = app.try_state::<DesktopState>() {
        if let Some(ref bridge) = state.log_bridge {
            bridge.stop();
        }
        tauri::async_runtime::block_on(state.supervisor.stop_graceful());
    }
    app.exit(0);
}

#[tauri::command]
#[allow(dead_code)]
fn cmd_providers(state: tauri::State<DesktopState>) -> Result<Value, String> {
    let ui = &state.ui_state;
    let app = &state.app_state;
    let cfg = ui.config.read().clone();
    let mut providers: Vec<Value> = Vec::new();
    for (name, entry) in [
        ("openai", cfg.providers.openai.clone()),
        ("anthropic", cfg.providers.anthropic.clone()),
        ("gemini", cfg.providers.gemini.clone()),
    ] {
        if let Some(e) = entry {
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
                    autorouter_config::ApiFormat::Responses => "responses",
                }
            };
            providers.push(serde_json::json!({
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
    }
    for (name, e) in &cfg.providers.custom {
        // Mirror the HTTP /ui/providers dedupe: drop custom entries
        // whose id collides with a first-class slot.
        if autorouter_server::ui::FIRST_CLASS_IDS.contains(&name.as_str()) {
            continue;
        }
        let fmt = match e.api_format {
            autorouter_config::ApiFormat::Anthropic => "anthropic",
            autorouter_config::ApiFormat::Gemini => "gemini",
            autorouter_config::ApiFormat::OpenAI => "openai",
            autorouter_config::ApiFormat::Responses => "responses",
        };
        providers.push(serde_json::json!({
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
    let adapters = app.pipeline_adapters();
    let mut models: Vec<Value> = Vec::new();
    for a in adapters.iter() {
        for m in a.models().iter() {
            models.push(serde_json::json!({
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
            models.push(serde_json::json!({
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
    Ok(serde_json::json!({ "providers": providers, "models": models }))
}

#[tauri::command]
#[allow(dead_code)]
fn cmd_sessions(state: tauri::State<DesktopState>) -> Result<Value, String> {
    let list: Vec<Value> = state
        .app_state
        .sessions
        .list()
        .into_iter()
        .map(|se| {
            serde_json::json!({
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
    Ok(serde_json::json!({ "sessions": list }))
}

#[tauri::command]
#[allow(dead_code)]
fn cmd_settings_get(state: tauri::State<DesktopState>) -> Result<Value, String> {
    let cfg = state.ui_state.config.read().clone();
    // Never echo the bearer credential back to the webview.
    let v = serde_json::to_value(&cfg).map_err(|e| e.to_string())?;
    Ok(autorouter_server::ui::redact_auth_token_in_config(v))
}

#[tauri::command]
#[allow(dead_code)]
async fn cmd_settings_patch(
    app: AppHandle,
    state: tauri::State<'_, DesktopState>,
    patch: Value,
) -> Result<Value, String> {
    let settings_patch: autorouter_server::ui::SettingsPatch =
        serde_json::from_value(patch).map_err(|e| e.to_string())?;
    // R11: capture which fields changed BEFORE consuming
    // settings_patch in apply_settings_patch, so we can avoid
    // rebuilding the smart router on non-routing patches.
    let router_changed = settings_patch.defaults.is_some()
        || settings_patch.providers.openai.is_some()
        || settings_patch.providers.anthropic.is_some()
        || settings_patch.providers.gemini.is_some()
        || !settings_patch.providers.custom.is_empty();
    let new_state: autorouter_server::supervisor::RouterBuildState;
    {
        let mut cfg = state.ui_state.config.write();
        let store_guard = state.ui_state.secret_store.read();
        autorouter_server::ui::apply_settings_patch(
            &mut cfg,
            settings_patch,
            store_guard.as_ref(),
        )?;
        new_state = autorouter_server::supervisor::RouterBuildState {
            bind: cfg.server.bind.clone(),
            enable_cors: cfg.server.cors_enabled(),
            max_body_bytes: cfg.server.max_body_bytes,
            request_timeout_seconds: cfg.server.request_timeout_seconds,
            stream_idle_timeout_seconds: cfg.server.stream_idle_timeout_seconds,
        };
    }
    // M14: persist to disk via the same atomic-write path the HTTP
    // PATCH /ui/settings uses, and mirror important runtime values
    // into StorageHandle so they survive restarts.
    let cfg = state.ui_state.config.read().clone();
    if let Some(path) = state.ui_state.config_path.read().clone() {
        if let Err(e) = autorouter_server::ui::write_config_atomic_public(&path, &cfg) {
            autorouter_server::ui::LogLine::push(
                &state.ui_state.log_lines,
                "error",
                "ui",
                &format!("settings persistence failed: {e}"),
            );
        }
    }
    if let Some(storage) = state.app_state.storage.as_ref() {
        autorouter_server::ui::mirror_settings_to_storage(storage, &cfg);
    }
    // M8 (live settings): swap the new config into the running
    // AppState so non-bind fields (require_auth, timeouts, etc.)
    // take effect on the next request without a manual restart.
    state.app_state.replace_config(cfg.clone());
    // Rebuild the upstream set from the patched config so changes
    // to provider api_key_secret_id, base_url, or `enabled` flag
    // take effect on the next request. Mirrors the HTTP
    // PATCH /ui/settings path.
    let rebuild_cfg = state.app_state.current_config();
    let rebuild_store = state.ui_state.secret_store.read().clone();
    let new_set = autorouter_server::rebuild_upstreams(&rebuild_cfg, rebuild_store);
    state.app_state.replace_upstreams(new_set);
    // R11: rebuild the smart router only when the patch touched
    // routing-relevant fields (defaults, provider configs, or
    // custom providers). Mirrors the HTTP PATCH /ui/settings path.
    if router_changed {
        let smart_router = autorouter_server::build_smart_router(
            &state.app_state.pipeline,
            &cfg,
            (*state.app_state.health).clone(),
            &state.app_state.model_db.read(),
        );
        state.app_state.replace_router(smart_router);
        autorouter_server::model_db::trigger_scraping_if_needed(
            &state.app_state,
            &cfg,
            state.app_state.data_dir.as_ref(),
        );
    }
    autorouter_server::ui::LogLine::push(
        &state.ui_state.log_lines,
        "info",
        "ui",
        "Settings updated from desktop UI",
    );
    // If the bind changed, hot-rebind the gateway so it actually
    // listens on the new port. The previous TcpListener is shut
    // down and a fresh axum::serve task is spawned on the new
    // address. We await the rebind inline so errors propagate to
    // the caller — the HTTP PATCH /ui/settings handler follows
    // the same pattern (returns 500 on rebind failure).
    let new_bind_for_task = new_state.bind.clone();
    let cors_for_rebind = new_state.enable_cors;
    let app_state_for_task = state.app_state.clone();
    let ui_state_for_task = state.ui_state.clone();
    let bind_lock = state.bind.clone();
    let log_lines = state.ui_state.log_lines.clone();
    // R10: clone the supervisor once for the rebind call (it takes
    // `self` by value), and once more for the closure since
    // sync_router_state also passes the second clone to the closure.
    let supervisor = state.supervisor.clone();
    let supervisor_for_closure = supervisor.clone();
    match supervisor
        .sync_router_state(new_state, move || async move {
            autorouter_server::build_router_with_ui(
                app_state_for_task.clone(),
                ui_state_for_task.clone(),
                cors_for_rebind,
                Some(supervisor_for_closure.clone()),
            )
        })
        .await
    {
        Ok(RebindOutcome::Rebound) => {
            autorouter_server::ui::LogLine::push(
                &log_lines,
                "info",
                "gateway",
                &format!("Hot-rebound gateway to {new_bind_for_task}"),
            );
            *bind_lock.write() = new_bind_for_task.clone();
            // R10: notify the webview so the dashboard chip
            // updates immediately instead of waiting for the
            // 5-second poll. The UI listens for `gateway-ready`
            // and re-emits the same event on rebind for symmetry.
            let _ = app.emit("gateway-ready", new_bind_for_task.clone());
        }
        Ok(RebindOutcome::AlreadyOnTarget) => {
            // No rebind necessary; nothing to do.
        }
        Err(e) => {
            autorouter_server::ui::LogLine::push(
                &log_lines,
                "error",
                "gateway",
                &format!("Hot-rebind to {new_bind_for_task} failed: {e}"),
            );
            return Err(format!("Hot-rebind to {new_bind_for_task} failed: {e}"));
        }
    }
    // Redact the bearer credential from the response.
    let v = serde_json::to_value(&cfg).map_err(|e| e.to_string())?;
    Ok(autorouter_server::ui::redact_auth_token_in_config(v))
}

/// PATCH `/ui/settings` with a single `{defaults: {default_provider,
/// default_model}}` payload and persist the change. Used by the
/// provider/model switcher overlay so a single keystroke can flip
/// the active default and have it survive a restart.
///
/// This is a thin wrapper around `cmd_settings_patch` — same atomic
/// write path, same smart-router rebuild, same redaction. We keep it
/// as a separate command because the JS side wants a one-shot
/// `setDefaultProviderModel(provider, model)` that doesn't have to
/// know the nested `{defaults: {...}}` shape.
#[tauri::command]
#[allow(dead_code)]
fn set_default_provider_model(
    state: tauri::State<DesktopState>,
    provider: String,
    model: String,
) -> Result<Value, String> {
    let patch = serde_json::json!({
        "defaults": {
            "default_provider": provider,
            "default_model": model,
        }
    });
    let settings_patch: autorouter_server::ui::SettingsPatch =
        serde_json::from_value(patch).map_err(|e| e.to_string())?;
    let cfg = {
        let mut cfg = state.ui_state.config.write();
        let store_guard = state.ui_state.secret_store.read();
        autorouter_server::ui::apply_settings_patch(
            &mut cfg,
            settings_patch,
            store_guard.as_ref(),
        )?;
        cfg.clone()
    };
    // Persist + mirror to storage so the next boot sees the new
    // defaults (the storage handle also restores them at startup —
    // see the `get_setting` calls in `build_gateway`).
    if let Some(path) = state.ui_state.config_path.read().clone() {
        if let Err(e) = autorouter_server::ui::write_config_atomic_public(&path, &cfg) {
            autorouter_server::ui::LogLine::push(
                &state.ui_state.log_lines,
                "error",
                "ui",
                &format!("switcher persistence failed: {e}"),
            );
        }
    }
    if let Some(storage) = state.app_state.storage.as_ref() {
        autorouter_server::ui::mirror_settings_to_storage(storage, &cfg);
    }
    // R11: rebuild the smart router so the next request uses the
    // new defaults, mirroring the HTTP PATCH path.
    let smart_router = autorouter_server::build_smart_router(
        &state.app_state.pipeline,
        &cfg,
        (*state.app_state.health).clone(),
        &state.app_state.model_db.read(),
    );
    state.app_state.replace_router(smart_router);
    autorouter_server::model_db::trigger_scraping_if_needed(
        &state.app_state,
        &cfg,
        state.app_state.data_dir.as_ref(),
    );
    autorouter_server::ui::LogLine::push(
        &state.ui_state.log_lines,
        "info",
        "switcher",
        &format!(
            "Default switched to provider={} model={}",
            cfg.defaults.default_provider, cfg.defaults.default_model
        ),
    );
    // Return only the bits the overlay actually needs — the full
    // config would leak any redacted fields and is also wasteful
    // over IPC. The JS side reads `defaults.{default_provider,
    // default_model}` from this shape.
    Ok(serde_json::json!({
        "ok": true,
        "defaults": {
            "default_provider": cfg.defaults.default_provider,
            "default_model": cfg.defaults.default_model,
        }
    }))
}

#[tauri::command]
#[allow(dead_code)]
fn cmd_logs(
    state: tauri::State<DesktopState>,
    since: Option<i64>,
    limit: Option<usize>,
    level: Option<String>,
) -> Result<Value, String> {
    let limit = limit.unwrap_or(500).clamp(1, 5_000);
    let g = state.ui_state.log_lines.read();
    // M22: compute start index from `since` cursor first, then clamp
    // to limit so the slice is bounded without discarding cursor-based entries.
    let since_start = if let Some(since) = since {
        // M22: use `<=` so polls return lines strictly newer than
        // `since`, matching the headless `get_logs` behaviour.
        g.partition_point(|l| l.ts.timestamp_millis() <= since)
    } else {
        0
    };
    let start = if g.len() > limit && since_start > g.len() - limit {
        // Truncate from the head so the slice never exceeds `limit`
        // entries, but only when the `since` cursor is past the
        // truncation point.
        g.len() - limit
    } else {
        since_start
    };
    let mut slice: Vec<autorouter_server::ui::LogLine> = g[start..].to_vec();
    if let Some(level) = level {
        slice.retain(|l| l.level.eq_ignore_ascii_case(&level));
    }
    let last_ts = slice.last().map(|l| l.ts.timestamp_millis()).unwrap_or(0);
    Ok(serde_json::json!({ "lines": slice, "next_since": last_ts }))
}

#[tauri::command]
#[allow(dead_code)]
async fn cmd_restart(
    app: AppHandle,
    state: tauri::State<'_, DesktopState>,
) -> Result<Value, String> {
    autorouter_server::ui::LogLine::push(
        &state.ui_state.log_lines,
        "info",
        "ui",
        "Restart requested from UI",
    );
    // Hot-rebind using the latest `server.bind` from the
    // in-memory config. If the bind has not changed since the
    // last successful bind, the supervisor is a no-op and the
    // running task keeps serving - this is the desired behaviour
    // when the operator hits "Restart server" without editing
    // any field. TcpListener::bind is async so we await the
    // rebind inline — errors propagate to the caller.
    let supervisor = state.supervisor.clone();
    let cfg = state.ui_state.config.read().clone();
    let new_bind = cfg.server.bind.clone();
    let cors_for_rebind = cfg.server.cors_enabled();
    let app_state_for_task = state.app_state.clone();
    let ui_state_for_task = state.ui_state.clone();
    let bind_lock = state.bind.clone();
    let log_lines = state.ui_state.log_lines.clone();
    // R10: clone the supervisor once for the rebind call (it takes
    // `self` by value), and once more for the closure.
    let supervisor_for_task = supervisor.clone();
    let supervisor_for_closure = supervisor_for_task.clone();
    match supervisor_for_task
        .rebind_if_needed(&new_bind, move || async move {
            autorouter_server::build_router_with_ui(
                app_state_for_task.clone(),
                ui_state_for_task.clone(),
                cors_for_rebind,
                Some(supervisor_for_closure.clone()),
            )
        })
        .await
    {
        Ok(RebindOutcome::Rebound) => {
            autorouter_server::ui::LogLine::push(
                &log_lines,
                "info",
                "gateway",
                &format!("Restart: rebound gateway to {new_bind}"),
            );
            *bind_lock.write() = new_bind.clone();
            // R10: notify the webview so the dashboard chip
            // updates immediately. The webview listens for
            // `gateway-ready` on initial mount and re-emits it
            // whenever the bind changes.
            let _ = app.emit("gateway-ready", new_bind.clone());
        }
        Ok(RebindOutcome::AlreadyOnTarget) => {
            autorouter_server::ui::LogLine::push(
                &log_lines,
                "info",
                "gateway",
                &format!("Restart: already serving on {new_bind}"),
            );
        }
        Err(e) => {
            autorouter_server::ui::LogLine::push(
                &log_lines,
                "error",
                "gateway",
                &format!("Restart failed: {e}"),
            );
            return Err(format!("Restart failed: {e}"));
        }
    }
    Ok(serde_json::json!({ "ok": true }))
}

#[tauri::command]
#[allow(dead_code)]
fn cmd_server_info(state: tauri::State<DesktopState>) -> Result<Value, String> {
    let cfg = state.ui_state.config.read().clone();
    // Prefer the live supervisor address so the dashboard reflects
    // the actual socket, not a stale value from a prior restart.
    let bind = state
        .supervisor
        .current_bind()
        .unwrap_or_else(|| state.bind.read().clone());
    Ok(serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "build": { "target": std::env::consts::ARCH, "os": std::env::consts::OS },
        "config": {
            "bind": bind,
            "max_body_bytes": cfg.server.max_body_bytes,
            "request_timeout_seconds": cfg.server.request_timeout_seconds,
            "stream_idle_timeout_seconds": cfg.server.stream_idle_timeout_seconds,
            "enable_cors": cfg.server.enable_cors.unwrap_or(true),
            "require_auth": cfg.server.require_auth.unwrap_or(false),
        }
    }))
}

#[tauri::command]
#[allow(dead_code)]
fn cmd_routing(state: tauri::State<DesktopState>) -> Result<Value, String> {
    let cfg = state.ui_state.config.read().clone();
    Ok(serde_json::json!({
        "rules": cfg.routing.rules,
        "default_tags": cfg.routing.default_tags,
    }))
}

#[tauri::command]
#[allow(dead_code)]
fn cmd_routing_patch(state: tauri::State<DesktopState>, patch: Value) -> Result<Value, String> {
    #[derive(serde::Deserialize)]
    struct RoutingPatch {
        rules: Option<Vec<Value>>,
        default_tags: Option<Vec<String>>,
    }
    let p: RoutingPatch = serde_json::from_value(patch).map_err(|e| e.to_string())?;
    {
        let mut cfg = state.ui_state.config.write();
        if let Some(rules) = p.rules {
            cfg.routing.rules = rules;
        }
        if let Some(t) = p.default_tags {
            cfg.routing.default_tags = t;
        }
    }
    let cfg = state.ui_state.config.read().clone();
    if let Some(path) = state.ui_state.config_path.read().clone() {
        if let Err(e) = autorouter_server::ui::write_config_atomic_public(&path, &cfg) {
            autorouter_server::ui::LogLine::push(
                &state.ui_state.log_lines,
                "error",
                "ui",
                &format!("Persisted routing failed: {e}"),
            );
        } else {
            autorouter_server::ui::LogLine::push(
                &state.ui_state.log_lines,
                "info",
                "ui",
                &format!("Persisted routing to {}", path.display()),
            );
        }
    }
    state.app_state.replace_config(cfg.clone());
    // R11: rebuild the smart router so new rules are live without
    // a process restart.
    let smart_router = autorouter_server::build_smart_router(
        &state.app_state.pipeline,
        &cfg,
        (*state.app_state.health).clone(),
        &state.app_state.model_db.read(),
    );
    state.app_state.replace_router(smart_router);
    autorouter_server::model_db::trigger_scraping_if_needed(
        &state.app_state,
        &cfg,
        state.app_state.data_dir.as_ref(),
    );
    Ok(serde_json::json!({
        "rules": cfg.routing.rules,
        "default_tags": cfg.routing.default_tags,
    }))
}

#[tauri::command]
#[allow(dead_code)]
fn cmd_health(state: tauri::State<DesktopState>) -> Result<Value, String> {
    let providers: Vec<Value> = [
        autorouter_core::ProviderKind::OpenAI,
        autorouter_core::ProviderKind::Anthropic,
        autorouter_core::ProviderKind::Gemini,
    ]
    .iter()
    .map(|k| {
        let snap = state.app_state.health.snapshot(*k);
        serde_json::json!({
            "provider": k.to_string(),
            "samples": snap.samples,
            "success_rate": snap.success_rate,
            "avg_latency_ms": snap.avg_latency_ms,
            "score": snap.score,
        })
    })
    .collect();
    Ok(serde_json::json!({ "providers": providers }))
}

#[tauri::command]
#[allow(dead_code)]
fn cmd_secrets(state: tauri::State<DesktopState>) -> Result<Value, String> {
    let store = state.ui_state.secret_store.read().clone();
    match store {
        None => Ok(serde_json::json!({
            "backend": null,
            "list_supported": false,
            "ids": Value::Null,
        })),
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
            Ok(serde_json::json!({
                "backend": backend,
                "list_supported": list_supported,
                "ids": ids,
            }))
        }
    }
}

#[tauri::command]
#[allow(dead_code)]
fn cmd_secret_get(state: tauri::State<DesktopState>, id: String) -> Result<Value, String> {
    let store = state.ui_state.secret_store.read().clone();
    let val = if let Some(env_name) = id.strip_prefix("env:") {
        std::env::var(env_name).ok()
    } else if let Some(store) = store {
        let secret_id = id.strip_prefix("keychain:").unwrap_or(&id).to_string();
        store.get(&secret_id.into()).ok().map(|sec| sec.value)
    } else {
        None
    };
    match val {
        Some(v) => Ok(serde_json::json!({ "value": v })),
        None => Err(format!("secret not found: {id}")),
    }
}

#[tauri::command]
#[allow(dead_code)]
fn cmd_secret_put(
    state: tauri::State<DesktopState>,
    id: String,
    value: String,
) -> Result<Value, String> {
    let store_guard = state.ui_state.secret_store.read().clone();
    let Some(store) = store_guard else {
        return Err("secret store not available".to_string());
    };
    let secret_id = id.strip_prefix("keychain:").unwrap_or(&id).to_string();
    let secret = autorouter_config::Secret::new(secret_id.clone(), value);
    store.put(secret).map_err(|e| e.to_string())?;
    tracing::info!(id = %id, "secret stored via desktop UI");
    Ok(serde_json::json!({ "ok": true, "id": id }))
}

#[tauri::command]
#[allow(dead_code)]
fn cmd_events(
    state: tauri::State<DesktopState>,
    provider: Option<String>,
    limit: Option<usize>,
) -> Result<Value, String> {
    let limit = limit.unwrap_or(100).clamp(1, 1000) as u32;
    let provider_filter = provider.as_deref().unwrap_or("");
    let storage_guard = state.ui_state.storage.read();
    let Some(storage) = storage_guard.as_ref() else {
        return Ok(serde_json::json!({
            "events": [],
            "note": "storage not enabled; no events captured"
        }));
    };
    let events = if provider_filter.is_empty() {
        // Pull events from every configured provider — built-in
        // slots AND custom providers (`openrouter`, `groq`, ...).
        // Previously this only queried the three built-ins and any
        // custom-provider traffic was invisible to the desktop UI.
        let mut kinds: Vec<String> = vec![
            "openai".to_string(),
            "anthropic".to_string(),
            "gemini".to_string(),
        ];
        for name in state.ui_state.config.read().providers.custom.keys() {
            let candidate = name.to_string();
            if !kinds.iter().any(|k| k == &candidate) {
                kinds.push(candidate);
            }
        }
        let mut all = Vec::new();
        for k in &kinds {
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
            .map_err(|e| e.to_string())?
    };
    Ok(serde_json::json!({
        "events": events.into_iter().map(|e| serde_json::json!({
            "provider": e.provider,
            "model": e.model,
            "kind": e.kind,
            "status": e.status,
            "latency_ms": e.latency_ms,
            "error": e.error,
            "created_at": e.created_at,
            "request_id": e.request_id,
            "session_id": e.session_id,
            "source_provider": e.source_provider,
            "input_tokens": e.input_tokens,
            "output_tokens": e.output_tokens,
            "cache_read_tokens": e.cache_read_tokens,
            "cache_write_tokens": e.cache_write_tokens,
            "reasoning_tokens": e.reasoning_tokens,
        })).collect::<Vec<_>>()
    }))
}

#[tauri::command]
#[allow(dead_code)]
fn cmd_analytics(state: tauri::State<DesktopState>) -> Result<Value, String> {
    let storage_guard = state.ui_state.storage.read();
    let mut kinds: Vec<String> = vec![
        "openai".to_string(),
        "anthropic".to_string(),
        "gemini".to_string(),
    ];
    for name in state.ui_state.config.read().providers.custom.keys() {
        let candidate = name.to_string();
        if !kinds.iter().any(|k| k == &candidate) {
            kinds.push(candidate);
        }
    }
    let mut all_events: Vec<autorouter_config::ProviderEvent> = Vec::new();
    if let Some(storage) = storage_guard.as_ref() {
        for k in &kinds {
            if let Ok(rows) = storage.recent_provider_events(k, 5_000) {
                all_events.extend(rows);
            }
        }
    }
    drop(storage_guard);
    let mut total_requests = 0u64;
    let mut total_failures = 0u64;
    let mut total_rate_limit_hits = 0u64;
    let mut total_input_tokens = 0u64;
    let mut total_output_tokens = 0u64;
    let mut total_cache_read_tokens = 0u64;
    let mut total_cache_write_tokens = 0u64;
    let mut total_reasoning_tokens = 0u64;
    use std::collections::BTreeMap;
    // Bucket shape: (requests, failures, input, output, cache_read,
    // cache_write, reasoning). Mirrors the HTTP /ui/analytics
    // endpoint so the dashboard renders the same numbers through
    // either transport.
    type AnalyticsBucket = (u64, u64, u64, u64, u64, u64, u64);
    let mut by_provider: BTreeMap<String, AnalyticsBucket> = BTreeMap::new();
    let mut by_model: BTreeMap<String, AnalyticsBucket> = BTreeMap::new();
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
        let entry = by_provider
            .entry(e.provider.clone())
            .or_insert((0, 0, 0, 0, 0, 0, 0));
        entry.0 += 1;
        if e.status >= 400 {
            entry.1 += 1;
        }
        entry.2 += e.input_tokens;
        entry.3 += e.output_tokens;
        entry.4 += e.cache_read_tokens;
        entry.5 += e.cache_write_tokens;
        entry.6 += e.reasoning_tokens;
        let mentry = by_model
            .entry(e.model.clone())
            .or_insert((0, 0, 0, 0, 0, 0, 0));
        mentry.0 += 1;
        if e.status >= 400 {
            mentry.1 += 1;
        }
        mentry.2 += e.input_tokens;
        mentry.3 += e.output_tokens;
        mentry.4 += e.cache_read_tokens;
        mentry.5 += e.cache_write_tokens;
        mentry.6 += e.reasoning_tokens;
    }
    let by_provider_out: Vec<Value> = by_provider
        .into_iter()
        .map(|(p, (r, f, i, o, cr, cw, rs))| {
            serde_json::json!({
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
            serde_json::json!({
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
    Ok(serde_json::json!({
        "total_requests": total_requests,
        "total_failures": total_failures,
        "total_input_tokens": total_input_tokens,
        "total_output_tokens": total_output_tokens,
        "total_cache_read_tokens": total_cache_read_tokens,
        "total_cache_write_tokens": total_cache_write_tokens,
        "total_reasoning_tokens": total_reasoning_tokens,
        "total_rate_limit_hits": total_rate_limit_hits,
        "by_provider": by_provider_out,
        "by_model": by_model_out,
        "events_examined": all_events.len(),
    }))
}

#[tauri::command]
#[allow(dead_code)]
fn cmd_debug(state: tauri::State<DesktopState>) -> Result<Value, String> {
    let cfg = state.ui_state.config.read().clone();
    let started = *state.ui_state.start_time.read();
    let uptime = (chrono::Utc::now() - started).num_seconds().max(0);
    let bind = state
        .supervisor
        .current_bind()
        .unwrap_or_else(|| state.bind.read().clone());
    let env: Vec<Value> = std::env::vars()
        .filter(|(k, _)| k.starts_with("AUTOROUTER_"))
        // Scrub the bearer credential from AUTOROUTER_AUTH_TOKEN.
        .map(|(k, v)| {
            if k == "AUTOROUTER_AUTH_TOKEN" {
                serde_json::json!({ "key": k, "value": "***", "redacted": true })
            } else {
                serde_json::json!({ "key": k, "value": v })
            }
        })
        .collect();
    let mut cfg_value = serde_json::to_value(&cfg).map_err(|e| e.to_string())?;
    // Same redaction as HTTP /ui/debug.
    cfg_value = autorouter_server::ui::redact_auth_token_in_config(cfg_value);
    Ok(serde_json::json!({
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
    }))
}

#[tauri::command]
#[allow(dead_code)]
fn cmd_tool_profiles(_state: tauri::State<DesktopState>) -> Result<Value, String> {
    // TODO: Read [tool_profiles] section from config.toml and return real data.
    Ok(serde_json::json!({
        "profiles": [],
        "hint": "Tool profiles are stored under [tool_profiles] in config.toml"
    }))
}

#[tauri::command]
#[allow(dead_code)]
fn cmd_tool_profile_save(
    state: tauri::State<DesktopState>,
    profile: Value,
) -> Result<Value, String> {
    #[derive(serde::Deserialize)]
    struct SaveToolProfile {
        name: String,
        description: Option<String>,
        schema: Value,
    }
    // TODO: Persist the profile to config.toml [tool_profiles] and rebuild the tool registry.
    let p: SaveToolProfile = serde_json::from_value(profile).map_err(|e| e.to_string())?;
    autorouter_server::ui::LogLine::push(
        &state.ui_state.log_lines,
        "info",
        "ui",
        &format!("Tool profile saved: {}", p.name),
    );
    Ok(serde_json::json!({ "ok": true, "name": p.name }))
}

#[tauri::command]
#[allow(dead_code)]
fn cmd_tool_test(
    _state: tauri::State<DesktopState>,
    name: String,
    input: Value,
) -> Result<Value, String> {
    // TODO: Look up the named tool profile and execute a real test call against a configured model.
    Ok(serde_json::json!({
        "ok": true,
        "name": name,
        "input": input,
        "message": "Tool test stub. Real validation runs in a follow-up.",
    }))
}

/// Probes a configured provider with a minimal chat-completion request
/// to verify the base URL + secret resolve to a working upstream.
/// Runs on `tauri::async_runtime`.
#[tauri::command]
#[allow(dead_code)]
async fn cmd_provider_test(
    state: tauri::State<'_, DesktopState>,
    id: String,
    model: Option<String>,
) -> Result<Value, String> {
    let cfg = state.ui_state.config.read().clone();
    // 1. Resolve the entry by id (same logic as the HTTP handler).
    let (provider_id, entry): (String, autorouter_config::ProviderEntry) = match id.as_str() {
        "openai" => match cfg.providers.openai {
            Some(ref e) => ("openai".to_string(), e.clone()),
            None => {
                return Ok(test_provider_error_local(
                    "missing",
                    "openai provider is not configured",
                ))
            }
        },
        "anthropic" => match cfg.providers.anthropic {
            Some(ref e) => ("anthropic".to_string(), e.clone()),
            None => {
                return Ok(test_provider_error_local(
                    "missing",
                    "anthropic provider is not configured",
                ))
            }
        },
        "gemini" => match cfg.providers.gemini {
            Some(ref e) => ("gemini".to_string(), e.clone()),
            None => {
                return Ok(test_provider_error_local(
                    "missing",
                    "gemini provider is not configured",
                ))
            }
        },
        other => match cfg.providers.custom.get(other) {
            Some(e) => (other.to_string(), e.clone()),
            None => {
                return Ok(test_provider_error_local(
                    "missing",
                    &format!("provider `{other}` is not configured"),
                ))
            }
        },
    };
    // 2. Build a minimal probe body in the matching wire format.
    let model: String = model
        .or_else(|| entry.model_allowlist.first().cloned())
        .unwrap_or_else(|| match entry.api_format {
            autorouter_config::ApiFormat::Anthropic => "claude-3-5-haiku-latest".to_string(),
            autorouter_config::ApiFormat::Gemini => "gemini-1.5-flash".to_string(),
            autorouter_config::ApiFormat::OpenAI => "gpt-4o-mini".to_string(),
            autorouter_config::ApiFormat::Responses => "gpt-4o-mini".to_string(),
        });
    let kind = autorouter_server::upstream::api_format_to_kind(entry.api_format);
    let body: Value = match kind {
        autorouter_core::ProviderKind::Anthropic => serde_json::json!({
            "model": model,
            "max_tokens": 1,
            "messages": [{ "role": "user", "content": "ping" }],
        }),
        autorouter_core::ProviderKind::Gemini => serde_json::json!({
            "model": model,
            "contents": [{ "role": "user", "parts": [{ "text": "ping" }] }],
            "generationConfig": { "maxOutputTokens": 1 },
        }),
        _ => serde_json::json!({
            "model": model,
            "max_tokens": 1,
            "messages": [{ "role": "user", "content": "ping" }],
        }),
    };
    // 3. Resolve the secret and build a one-shot HttpUpstream.
    let store = state.ui_state.secret_store.read().clone();
    let auth_value = autorouter_server::upstream::resolve_secret(
        entry.api_key_secret_id.as_deref(),
        store.as_ref(),
    );
    if entry.api_key_secret_id.is_some() && auth_value.is_none() && !entry.base_url.is_empty() {
        return Ok(test_provider_error_local(
            "missing_secret",
            "API key secret reference is set but the value could not be resolved",
        ));
    }
    let timeout = std::time::Duration::from_secs(
        state
            .app_state
            .current_config()
            .server
            .request_timeout_seconds,
    );
    let http_cfg = autorouter_server::upstream::HttpUpstreamConfig::from_entry(
        &entry, kind, auth_value, timeout,
    );
    let upstream = match autorouter_server::upstream::HttpUpstream::new(http_cfg) {
        Ok(u) => u,
        Err(e) => {
            return Ok(test_provider_error_local(
                "build_failed",
                &format!("failed to build upstream client: {e}"),
            ))
        }
    };
    // 4. Fire the probe on Tauri's async runtime (which is tokio).
    // We block on it here so the Tauri command signature stays
    // synchronous; the probe is bounded by `request_timeout_seconds`
    // so worst-case latency is one configured timeout.
    let started = std::time::Instant::now();
    let result = upstream.send(&body).await;
    let latency_ms = started.elapsed().as_millis() as u64;
    match result {
        Ok(resp) => {
            autorouter_server::ui::LogLine::push(
                &state.ui_state.log_lines,
                "info",
                "ui",
                &format!("Provider test OK: id={provider_id} model={model} status={} latency_ms={latency_ms}", resp.status),
            );
            Ok(serde_json::json!({
                "ok": true,
                "provider": provider_id,
                "model": model,
                "status": resp.status,
                "latency_ms": latency_ms,
            }))
        }
        Err(e) => {
            autorouter_server::ui::LogLine::push(
                &state.ui_state.log_lines,
                "warn",
                "ui",
                &format!("Provider test FAILED: id={provider_id} model={model} latency_ms={latency_ms} error={e}"),
            );
            Ok(serde_json::json!({
                "ok": false,
                "provider": provider_id,
                "model": model,
                "latency_ms": latency_ms,
                "error": e.to_string(),
            }))
        }
    }
}

/// Build the standard error response shape for `cmd_provider_test`.
fn test_provider_error_local(kind: &str, message: &str) -> Value {
    serde_json::json!({
        "ok": false,
        "error": message,
        "error_kind": kind,
    })
}

#[tauri::command]
#[allow(dead_code)]
fn cmd_import_config(state: tauri::State<DesktopState>, text: String) -> Result<Value, String> {
    let path = state
        .ui_state
        .config_path
        .read()
        .clone()
        .ok_or_else(|| "no config path set".to_string())?;
    // Use atomic temp-file + rename to avoid corruption on crash.
    let tmp_ext = format!(".import.{}", std::process::id());
    let tmp_path = path.with_extension(
        path.extension()
            .map(|e| {
                let mut s = e.to_string_lossy().to_string();
                s.push_str(&tmp_ext);
                s
            })
            .unwrap_or_else(|| tmp_ext.trim_start_matches('.').to_string()),
    );
    std::fs::write(&tmp_path, &text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp_path, &path).map_err(|e| e.to_string())?;
    autorouter_server::ui::LogLine::push(
        &state.ui_state.log_lines,
        "info",
        "ui",
        &format!("Imported config to {}", path.display()),
    );
    Ok(serde_json::json!({ "ok": true, "path": path }))
}

#[tauri::command]
#[allow(dead_code)]
fn cmd_export_config(state: tauri::State<DesktopState>) -> Result<String, String> {
    let path = state
        .ui_state
        .config_path
        .read()
        .clone()
        .ok_or_else(|| "no config path set".to_string())?;
    let body = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    // Same redaction as the HTTP /ui/export endpoint.
    Ok(autorouter_server::ui::redact_auth_token_in_toml(&body))
}

#[tauri::command]
#[allow(dead_code)]
fn cmd_check_update(_state: tauri::State<DesktopState>) -> Result<Value, String> {
    // TODO: Implement real update check against GitHub releases API or configured update channel.
    Ok(serde_json::json!({
        "current_version": env!("CARGO_PKG_VERSION"),
        "latest_version": env!("CARGO_PKG_VERSION"),
        "update_available": false,
        "release_notes": "Auto-update check requires network access. The current build is the latest published version.",
        "release_url": null,
        "published_at": null,
        "can_self_update": false,
    }))
}

/// Boot the gateway and merge the UI routes into the router. The
/// returned `GatewaySupervisor` is what the HTTP `patch_settings` and
/// `restart` handlers (and the Tauri `cmd_settings_patch` command)
/// use to hot-rebind the listener when the bind changes.
pub type Gateway = (
    axum::Router,
    AppState,
    UiState,
    autorouter_config::AppConfig,
    String,
    autorouter_server::GatewaySupervisor,
);

pub type GatewayError = Box<dyn std::error::Error + Send + Sync>;

pub fn build_gateway() -> Result<Gateway, GatewayError> {
    let paths = ProjectPaths::resolve()
        .unwrap_or_else(|| ProjectPaths::under_root(std::path::Path::new(".")));
    let mut config = ConfigLoader::from_standard_chain(&paths)?;
    let pipeline = TranslationPipeline::new()
        .register(Arc::new(OpenAiChatAdapter::new()))
        .register(Arc::new(OpenAiResponsesAdapter::new()))
        .register(Arc::new(AnthropicAdapter::new()))
        .register(Arc::new(GeminiAdapter::new()));

    // Build the real upstream map. Providers that are not configured
    // (no base URL) fall back to MockUpstream so the gateway stays
    // usable in offline mode.
    //
    // Per manual.md the default is the OS keychain;
    // AUTOROUTER_SECRET_STORE is an override, not a fallback.
    let secret_store_kind = std::env::var("AUTOROUTER_SECRET_STORE")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "keychain".to_string());
    let secret_store = autorouter_config::build_secret_store(
        &secret_store_kind,
        std::env::var_os("AUTOROUTER_SECRET_FILE")
            .as_deref()
            .map(std::path::Path::new),
    );
    let upstreams = autorouter_server::build_upstreams(&config, Some(secret_store.clone()));
    let custom_upstreams = upstreams.custom.clone();
    let upstreams = upstreams.built_in;

    // Smart router: register capabilities from the adapters, add the
    // user-configured rules plus a default fallback rule, and wrap it
    // in a HealthTracker.
    // Open the SQLite store BEFORE constructing AppState so the runtime
    // settings persisted in storage (auth_token, default_provider,
    // default_model) are overlaid onto `config` before AppState takes
    // its first snapshot. Previously AppState was built at line 1308
    // before the overlay loop ran at lines 1337-1347, so any code
    // reading `AppState.config.server.auth_token` got the pre-overlay
    // value (the file value, not the persisted runtime value).
    let db_path = if config.storage.data_dir.is_empty() {
        paths.data_dir.join(&config.storage.database_file)
    } else {
        std::path::PathBuf::from(&config.storage.data_dir).join(&config.storage.database_file)
    };
    let storage_handle = match autorouter_server::StorageHandle::open(db_path.clone()) {
        Ok(h) => Some(Arc::new(h)),
        Err(e) => {
            tracing::warn!(error = %e, "failed to open storage; provider events will not be persisted");
            None
        }
    };
    // Overlay runtime settings from the same handle we just opened
    // (avoids opening SQLite twice and racing the second handle with
    // the first).
    if let Some(h) = storage_handle.as_ref() {
        if let Ok(Some(v)) = h.get_setting("auth_token") {
            config.server.auth_token = Some(v);
        }
        if let Ok(Some(v)) = h.get_setting("default_provider") {
            config.defaults.default_provider = v;
        }
        if let Ok(Some(v)) = h.get_setting("default_model") {
            config.defaults.default_model = v;
        }
    }

    let model_db = Arc::new(parking_lot::RwLock::new(
        autorouter_server::model_db::filter_scraped_models(
            autorouter_server::model_db::ModelDb::load_or_default(&paths.data_dir),
            &config,
        ),
    ));

    // Smart router: register capabilities from the adapters, add the
    // user-configured rules plus a default fallback rule, and wrap it
    // in a HealthTracker. Built AFTER the storage overlay so it sees
    // the post-overlay `defaults.default_provider` /
    // `defaults.default_model`.
    let health = autorouter_router::HealthTracker::new();
    let smart_router =
        autorouter_server::build_smart_router(&pipeline, &config, health.clone(), &model_db.read());

    let app_state = AppState::with_router(
        config.clone(),
        pipeline,
        upstreams,
        Some(smart_router),
        health.clone(),
    )
    .with_custom_upstreams(custom_upstreams)
    .with_storage(storage_handle.clone())
    .with_model_db(model_db.clone())
    .with_data_dir(paths.data_dir.clone());

    autorouter_server::model_db::trigger_scraping_if_needed(&app_state, &config, &paths.data_dir);

    health.print_samples();
    // H10: remember the storage handle and db path so the quit
    // command (and any other shutdown trigger) can run the
    // configured shutdown backup. No backup is taken here at
    // startup time.
    let database_file = db_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("autorouter.db")
        .to_string();
    let _ = SHUTDOWN_HANDLES.set(ShutdownHandles {
        storage: storage_handle.clone(),
        database_file,
        backup_keep: config.storage.backup_keep,
        backup_dir: if config.storage.data_dir.is_empty() {
            db_path
                .parent()
                .map(|p| p.join("backups"))
                .unwrap_or_else(|| std::path::PathBuf::from("backups"))
        } else {
            std::path::PathBuf::from(&config.storage.data_dir).join("backups")
        },
        backup_on_shutdown: config.storage.backup_on_shutdown.unwrap_or(true),
    });

    let config_path = autorouter_server::user_config_path(&paths);
    // Create the gateway supervisor BEFORE building the router so
    // the HTTP `patch_settings` and `restart` handlers can route
    // rebinds through it. The supervisor is a cheap Arc internally,
    // so the call sites can clone it freely. It is also threaded
    // into `UiState.supervisor` so any future TUI-style surface
    // sharing the `UiState` can hot-rebind (mirrors the headless
    // binary's wiring in `autorouter-app/src/main.rs`).
    let supervisor = autorouter_server::GatewaySupervisor::new();
    let ui_state = UiState {
        config: Arc::new(parking_lot::RwLock::new(config.clone())),
        start_time: Arc::new(parking_lot::RwLock::new(chrono::Utc::now())),
        log_lines: Arc::new(parking_lot::RwLock::new(Vec::new())),
        config_path: Arc::new(parking_lot::RwLock::new(Some(config_path))),
        storage: Arc::new(parking_lot::RwLock::new(storage_handle)),
        secret_store: Arc::new(parking_lot::RwLock::new(Some(secret_store.clone()))),
        supervisor: Some(supervisor.clone()),
    };
    let router = autorouter_server::build_router_with_ui(
        app_state.clone(),
        ui_state.clone(),
        true,
        Some(supervisor.clone()),
    );
    // H10: backup_on_shutdown is now performed in the
    // RunEvent::ExitRequested handler below (true shutdown), not at
    // gateway build time. We just remember the storage handle here.
    let bind = config.server.bind.clone();
    Ok((router, app_state, ui_state, config, bind, supervisor))
}

/// Main entry point used by `main.rs`.
pub fn run() {
    // Install the in-process log sink before init_logging so
    // tracing events are captured from the first record!() call.
    install_log_sink();
    let _ = init_logging(LoggingConfig {
        level: "info".into(),
        json: false,
        file: None,
    });

    use tauri_plugin_global_shortcut::Shortcut;
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_opener::init())
        // Global keyboard shortcut for the provider/model switcher.
        //
        // The plugin is registered with a single global handler that
        // filters on the chord string. We deliberately register the
        // chord via the plugin builder (so the handler closure does
        // not need to know the chord and we get exactly one
        // registration per process — see the "DO NOT register more
        // than one global shortcut" rule).
        //
        // The handler only acts on `ShortcutState::Pressed` (not on
        // release), so the overlay does not flicker on key-up. We
        // also skip emitting when the webview's main window is
        // hidden — the tray is still active and the user may be in
        // the tray menu, in which case popping the overlay would
        // steal focus from a native menu. The webview side calls
        // `preventDefault()` on the keystroke that opened it, so
        // the chord does not leak into whatever focused control
        // was underneath at the time of the press (the React
        // `Switcher` overlay captures keys once it mounts).
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |app, shortcut, event| {
                    if event.state() != ShortcutState::Pressed {
                        return;
                    }
                    let switcher_shortcut = match SWITCHER_SHORTCUT.parse::<Shortcut>() {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::error!(error = %e, "Failed to parse switcher shortcut");
                            return;
                        }
                    };
                    if *shortcut != switcher_shortcut {
                        return;
                    }
                    // Only emit when the main webview exists. On a
                    // headless `autorouter-app` run there is no
                    // `main` window; the plugin still has to be
                    // registered for the chord to be valid, but the
                    // emit is a no-op.
                    let chord: String = shortcut.into_string();
                    tracing::debug!(chord = %chord, "switcher shortcut pressed");
                    if let Some(win) = app.get_webview_window("switcher") {
                        let _ = win.show();
                        let _ = win.set_focus();
                        let _ = win.emit(SWITCHER_EVENT, ());
                    } else {
                        let builder = tauri::WebviewWindowBuilder::new(
                            app,
                            "switcher",
                            tauri::WebviewUrl::App("index.html?page=switcher".into()),
                        )
                        .title("AutoRouter Switcher")
                        .inner_size(680.0, 420.0)
                        .resizable(false)
                        .decorations(false)
                        .transparent(true)
                        .always_on_top(true)
                        .center();

                        match builder.build() {
                            Ok(win) => {
                                let win_clone = win.clone();
                                win.on_window_event(move |event| {
                                    if let tauri::WindowEvent::Focused(focused) = event {
                                        if !*focused {
                                            let _ = win_clone.hide();
                                        }
                                    }
                                });
                                let _ = win.show();
                                let _ = win.set_focus();
                            }
                            Err(e) => {
                                tracing::error!(error = %e, "Failed to build switcher window on shortcut");
                            }
                        }
                    }
                })
                .build(),
        )
        .setup(|app| {
            // Bind the embedded gateway to localhost so the webview
            // (origin tauri://localhost) can call back to it without
            // being treated as cross-origin. This is a no-op when the
            // user has set AUTOROUTER_BIND explicitly.
            if std::env::var("AUTOROUTER_BIND").is_err() {
                // SAFETY: This runs in the Tauri setup hook before
                // any worker threads read this variable. Tauri's
                // Builder::build() runs first but does not start
                // reading AUTOROUTER_BIND on any thread.
                std::env::set_var("AUTOROUTER_BIND", "127.0.0.1:4073");
            }

            // Pre-build switcher window (hidden at start)
            let switcher_win = tauri::WebviewWindowBuilder::new(
                app,
                "switcher",
                tauri::WebviewUrl::App("index.html?page=switcher".into()),
            )
            .title("AutoRouter Switcher")
            .inner_size(680.0, 420.0)
            .resizable(false)
            .decorations(false)
            .transparent(true)
            .always_on_top(true)
            .center()
            .visible(false);

            match switcher_win.build() {
                Ok(win) => {
                    let win_clone = win.clone();
                    win.on_window_event(move |event| {
                        if let tauri::WindowEvent::Focused(focused) = event {
                            if !*focused {
                                let _ = win_clone.hide();
                            }
                        }
                    });
                }
                Err(e) => {
                    tracing::error!(error = %e, "Failed to build switcher window at startup");
                }
            }
            // Build the gateway in a blocking task so the Tauri
            // setup hook stays synchronous.
            let handle = app.handle().clone();
            let (router, app_state, ui_state, _config, bind, supervisor) =
                match tauri::async_runtime::block_on(async { build_gateway() }) {
                Ok(v) => v,
                Err(e) => return Err(Box::new(std::io::Error::other(format!("gateway: {e}")))),
            };

            // `build_gateway` already created the supervisor and wired
            // it into the router (so the HTTP `patch_settings` and
            // `restart` handlers can route rebinds through it). All
            // that remains here is to actually start the listener.
            // The setup hook is sync, so we drive the async start with
            // `block_on` - the bind is bounded by the kernel so the
            // worst case is a port-already-in-use error, not a
            // deadlock.
            let addr = tauri::async_runtime::block_on(async {
                supervisor.clone().start(router, &bind).await
            })
            .map_err(|e| {
                Box::new(std::io::Error::other(format!(
                    "gateway bind to {bind} failed: {e}"
                )))
            })?;
            tracing::info!(addr = %addr, "AutoRouter gateway listening");

            // Bound session growth for long-running desktop sessions.
            // Tauri's setup hook is sync, so spawn on Tauri's runtime
            // rather than calling `tokio::spawn` (which panics when no
            // current-thread Tokio runtime is entered).
            {
                let sessions = app_state.sessions.clone();
                let max_age = chrono::Duration::from_std(
                    autorouter_server::DEFAULT_SESSION_MAX_AGE,
                )
                .unwrap_or_else(|_| chrono::Duration::hours(24));
                tauri::async_runtime::spawn(autorouter_server::run_session_pruner(
                    sessions,
                    autorouter_server::DEFAULT_SESSION_PRUNE_INTERVAL,
                    max_age,
                    autorouter_server::DEFAULT_SESSION_CAP,
                ));
            }

            // LogBridge: the Tauri setup hook is sync, so we route
            // the bridge onto Tauri's global async runtime via
            // `start_on_tauri` instead of calling `tokio::spawn`.
            let log_bridge = LogBridge::start_on_tauri(
                |fut| {
                    tauri::async_runtime::spawn(fut);
                },
                ui_state.log_lines.clone(),
                std::time::Duration::from_millis(100),
            );

            // Build the desktop state and inject it. The
            // supervisor and the cached `bind` lock replace the
            // previous "capture-once" pattern so the dashboard
            // can show a fresh bind and a hot-rebind is possible.
            let state = DesktopState {
                config: ui_state.config.clone(),
                ui_state: ui_state.clone(),
                app_state,
                bind: Arc::new(parking_lot::RwLock::new(bind.clone())),
                supervisor: supervisor.clone(),
                started_at: *ui_state.start_time.read(),
                log_bridge: Some(log_bridge),
            };
            app.manage(state);

            // System tray.
            let menu = Menu::with_items(
                app,
                &[
                    &MenuItem::with_id(app, "show", "Open Dashboard", true, None::<&str>)?,
                    &MenuItem::with_id(app, "logs", "Open Logs", true, None::<&str>)?,
                    &MenuItem::with_id(app, "quit", "Quit AutoRouter", true, None::<&str>)?,
                ],
            )?;
            // Read the OS theme at startup so we can pick the right icon
            // before the tray is even visible. We use the main window as the
            // source of truth; it is the most reliable cross-platform API.
            // Falls back to Light on Linux where `theme()` may return None.
            let initial_theme = app
                .get_webview_window("main")
                .and_then(|w| w.theme().ok())
                .unwrap_or(tauri::Theme::Light);
            tracing::info!(theme = ?initial_theme, "startup: detected OS theme");

            let mut tray_builder = TrayIconBuilder::with_id("autorouter-tray")
                .menu(&menu)
                .tooltip("AutoRouter");
            // Embed and apply the correct tray icon for the current theme.
            if let Some(img) = tray_icon_for_theme(initial_theme) {
                tray_builder = tray_builder.icon(img);
            }
            tray_builder
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(win) = app.get_webview_window("main") {
                            let _ = win.show();
                            let _ = win.set_focus();
                        }
                    }
                    "logs" => {
                        let _ = app.emit("navigate", "logs");
                    }
                    "quit" => {
                        if let Some(handles) = SHUTDOWN_HANDLES.get() {
                            perform_shutdown_backup(handles);
                        }
                        // Gracefully stop the gateway supervisor so
                        // in-flight requests get a 1-second drain
                        // window before the Tauri runtime tears down
                        // the socket. Without this, an SSE stream
                        // mid-flight sees a TCP reset instead of a
                        // clean disconnect.
                        if let Some(state) = app.try_state::<DesktopState>() {
                            let supervisor = state.supervisor.clone();
                            if let Some(ref bridge) = state.log_bridge {
                                bridge.stop();
                            }
                            tauri::async_runtime::block_on(async move {
                                supervisor.stop_graceful().await;
                            });
                        }
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click { button, button_state, .. } = event {
                        if button == MouseButton::Left && button_state == MouseButtonState::Up {
                            if let Some(win) = tray.app_handle().get_webview_window("main") {
                                let _ = win.show();
                                let _ = win.set_focus();
                            }
                        }
                    }
                })
                .build(app)?;

            // Apply window icon for the startup theme so both the taskbar
            // pinned icon and the native title-bar icon match the OS mode.
            // This is a separate call from the tray update above because
            // the window icon API is on WebviewWindow, not TrayIcon.
            if let Some(win) = app.get_webview_window("main") {
                if let Some(img) = window_icon_for_theme(initial_theme) {
                    let _ = win.set_icon(img);
                }
            }

            // The LogBridge is now live — tracing events flow to the UI.
            tracing::info!(addr = %bind, "AutoRouter desktop started");
            // Notify the UI that the gateway is up.
            let _ = handle.emit("gateway-ready", bind.clone());
            // Register the global keyboard shortcut for the
            // provider/model switcher overlay. We do this AFTER the
            // webview is up so the handler's `get_webview_window`
            // call has something to find. The plugin was already
            // wired into the builder above; this call tells it
            // which chord(s) to listen for. Exactly one chord is
            // registered, per the "DO NOT register more than one
            // global shortcut" rule.
            //
            // The shortcut is registered AFTER the tray is built so
            // a failure here doesn't block the gateway — a Tauri
            // shortcut registration can fail on environments that
            // don't expose the platform hotkey API (rare but
            // possible on locked-down Linux desktops). We log and
            // continue; the user can still open the overlay from
            // the Dashboard "Switcher" hint or by clicking the
            // dashboard's Bind tile.
            match app.global_shortcut().register(SWITCHER_SHORTCUT) {
                Ok(_) => {
                    tracing::info!(
                        shortcut = SWITCHER_SHORTCUT,
                        "Global switcher shortcut registered"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        shortcut = SWITCHER_SHORTCUT,
                        "Failed to register global switcher shortcut; \
                         overlay can still be opened from the dashboard"
                    );
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            match event {
                WindowEvent::CloseRequested { api, .. } => {
                    let label = window.label();
                    if label == "main" {
                        let _ = window.hide();
                        api.prevent_close();
                    }
                }

                // ── OS theme changed at runtime ───────────────────────────────
                // Fires on Windows and macOS when the user toggles dark/light
                // mode in system settings, or the OS automatic schedule kicks in.
                // Does NOT fire reliably on Linux — the startup `initial_theme`
                // path above is the primary mechanism there.
                WindowEvent::ThemeChanged(new_theme) => {
                    tracing::info!(
                        window = %window.label(),
                        theme  = ?new_theme,
                        "OS theme changed — updating tray and window icons"
                    );
                    let app = window.app_handle();
                    // Update the system tray icon.
                    if let Some(tray) = app.tray_by_id("autorouter-tray") {
                        if let Some(img) = tray_icon_for_theme(*new_theme) {
                            let _ = tray.set_icon(Some(img));
                        }
                    }
                    // Update the native window (title-bar / taskbar) icon.
                    if let Some(img) = window_icon_for_theme(*new_theme) {
                        let _ = window.set_icon(img);
                    }
                }

                _ => {}
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_status,
            cmd_providers,
            cmd_sessions,
            cmd_settings_get,
            cmd_settings_patch,
            set_default_provider_model,
            cmd_logs,
            cmd_restart,
            cmd_server_info,
            cmd_routing,
            cmd_routing_patch,
            cmd_health,
            cmd_events,
            cmd_secrets,
            cmd_secret_get,
            cmd_secret_put,
            cmd_analytics,
            cmd_debug,
            cmd_tool_profiles,
            cmd_tool_profile_save,
            cmd_tool_test,
            cmd_provider_test,
            cmd_import_config,
            cmd_export_config,
            cmd_check_update,
            open_external,
            reveal_data_dir,
            quit_app,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if let RunEvent::ExitRequested { .. } = event {
                tracing::info!("ExitRequested — performing shutdown backup and stopping gateway");
                if let Some(handles) = SHUTDOWN_HANDLES.get() {
                    perform_shutdown_backup(handles);
                }
                if let Some(state) = app.try_state::<DesktopState>() {
                    let supervisor = state.supervisor.clone();
                    if let Some(ref bridge) = state.log_bridge {
                        bridge.stop();
                    }
                    tauri::async_runtime::block_on(async move {
                        supervisor.stop_graceful().await;
                    });
                }
            }
        });
}

// ── Theme-responsive icon bundle ─────────────────────────────────────────────
//
// Both icon variants are embedded directly into the binary at compile time
// via `include_bytes!` so the application is fully self-contained — no
// runtime file-path lookups, no missing-asset panics after install.
//
// Platform strategy
// ─────────────────
// Windows  Taskbar and tray track the OS theme.  We swap between
//          tray-dark.png (white icon, visible on dark taskbar) and
//          tray-light.png (black icon, visible on light taskbar) on
//          every WindowEvent::ThemeChanged.
//
// macOS    Menu-bar tray icon is already handled by the OS Template
//          convention when the filename ends in "Template".  We still
//          swap for the window (Dock) icon if desired.
//
// Linux    ThemeChanged does not fire reliably across all DEs, so the
//          icon is set once during setup from the detected startup theme.

/// Return the tray icon for the given OS theme, embedded at compile time.
/// Returns `None` only if the PNG bytes are corrupt (should never happen
/// with assets committed to source control).
fn tray_icon_for_theme(theme: tauri::Theme) -> Option<Image<'static>> {
    let bytes: &[u8] = match theme {
        // Dark OS → white icon so it is visible against the dark taskbar/menu bar.
        tauri::Theme::Dark => include_bytes!("../icons/tray-dark.png"),
        // Light OS → black icon so it is visible against the light taskbar/menu bar.
        tauri::Theme::Light => include_bytes!("../icons/tray-light.png"),
        // Defensive default for any future Theme variant Tauri adds.
        _ => include_bytes!("../icons/tray-light.png"),
    };
    Image::from_bytes(bytes).ok()
}

/// Return the window (title-bar / taskbar pinned) icon for the given theme.
/// Currently uses a single asset but is separated from the tray helper so
/// callers can swap to dedicated light/dark window icons without touching
/// the tray path.
fn window_icon_for_theme(theme: tauri::Theme) -> Option<Image<'static>> {
    // The existing 32x32 icon has a transparent background, so it reads
    // acceptably in both modes.  Swap to dedicated light/dark variants here
    // if you want fully theme-matched window icons in the future.
    let bytes: &[u8] = match theme {
        tauri::Theme::Dark => include_bytes!("../icons/tray-dark.png"),
        tauri::Theme::Light => include_bytes!("../icons/tray-light.png"),
        _ => include_bytes!("../icons/tray-light.png"),
    };
    Image::from_bytes(bytes).ok()
}
