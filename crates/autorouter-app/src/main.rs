#![deny(unused_crate_dependencies)]
//! AutoRouter application entry point.
//!
//! Reads the merged configuration, initialises logging and metrics,
//! opens (or creates) the SQLite storage, and starts the local HTTP
//! gateway. On Ctrl-C the storage is closed cleanly and, if
//! configured, the database is backed up.

use std::io::IsTerminal;
use std::sync::Arc;

use anyhow::{Context, Result};

mod tui;

use autorouter_config::{ConfigLoader, ProjectPaths};
use autorouter_observability::{
    init_logging, install_log_sink, record_request, validate_storage, LoggingConfig,
};
use autorouter_router::HealthTracker;
use autorouter_server::{storage::StorageHandle, AppState, LogBridge, TranslationPipeline};
use autorouter_translate::{
    AnthropicAdapter, GeminiAdapter, OpenAiChatAdapter, OpenAiResponsesAdapter,
};

use tokio::signal;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mut headless = false;
    for arg in &args {
        if arg == "-h" || arg == "--help" {
            println!("AutoRouter Gateway CLI");
            println!("Usage:");
            println!("  autorouter [options]");
            println!();
            println!("Options:");
            println!("  -h, --help      Show this help message");
            println!("  --headless      Run in headless mode (no interactive terminal UI)");
            return Ok(());
        } else if arg == "--headless" {
            headless = true;
        }
    }

    let paths = ProjectPaths::resolve()
        .unwrap_or_else(|| ProjectPaths::under_root(std::path::Path::new(".")));

    let mut config = ConfigLoader::from_standard_chain(&paths).context("loading configuration")?;
    validate_storage(&config.storage).context("validating storage")?;

    // Open the SQLite store (or skip if it is disabled by leaving
    // data_dir empty AND backup_on_shutdown off AND the database_file
    // name empty - a corner case; the loader fills sensible defaults).
    let db_path = if config.storage.data_dir.is_empty() {
        paths.data_dir.join(&config.storage.database_file)
    } else {
        std::path::PathBuf::from(&config.storage.data_dir).join(&config.storage.database_file)
    };
    // M9: log when the storage open fails instead of silently
    // swallowing the error. The gateway still works without
    // persistence, but the operator should know.
    let storage = match StorageHandle::open(db_path.clone()) {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::warn!(error = %e, "failed to open storage; provider events will not be persisted");
            None
        }
    };
    // If a previous run persisted runtime settings (e.g. the
    // optional bearer token), overlay them onto the in-memory
    // config before the gateway starts serving.
    if let Some(s) = storage.as_ref() {
        if let Ok(Some(v)) = s.get_setting("auth_token") {
            config.server.auth_token = Some(v);
        }
        if let Ok(Some(v)) = s.get_setting("default_provider") {
            config.defaults.default_provider = v;
        }
        if let Ok(Some(v)) = s.get_setting("default_model") {
            config.defaults.default_model = v;
        }
    }

    // Install the log sink and the bridge that copies entries into
    // the in-process UI buffer.
    let log_buffer = Arc::new(parking_lot::RwLock::new(Vec::new()));
    install_log_sink();
    // M15: 100ms keeps the Logs page snappy without burning CPU.
    // `start_on_tokio` uses the `#[tokio::main]` runtime that
    // wraps `async fn main()`.
    let log_bridge = LogBridge::start_on_tokio(
        &tokio::runtime::Handle::current(),
        log_buffer.clone(),
        std::time::Duration::from_millis(100),
    );

    init_logging(LoggingConfig {
        level: config.logging.level.clone(),
        json: config.logging.json.unwrap_or(false),
        file: config.logging.file.clone(),
    })
    .context("initialising logging")?;

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        bind = %config.server.bind,
        "AutoRouter starting"
    );

    let pipeline = TranslationPipeline::new()
        .register(Arc::new(OpenAiChatAdapter::new()))
        .register(Arc::new(OpenAiResponsesAdapter::new()))
        .register(Arc::new(AnthropicAdapter::new()))
        .register(Arc::new(GeminiAdapter::new()));

    let model_db = Arc::new(parking_lot::RwLock::new(
        autorouter_server::model_db::filter_scraped_models(
            autorouter_server::model_db::ModelDb::load_or_default(&paths.data_dir),
            &config,
        ),
    ));

    // M4: use the SAME HealthTracker for the smart router and the
    // app state so /ui/health and routing decisions see one view.
    let health = HealthTracker::new();
    let smart =
        autorouter_server::build_smart_router(&pipeline, &config, health.clone(), &model_db.read());

    // Per manual.md the documented default is the OS keychain;
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
    let upstream_set = autorouter_server::build_upstreams(&config, Some(secret_store.clone()));
    let custom_upstreams = upstream_set.custom.clone();
    let upstreams = upstream_set.built_in;
    let state_storage = storage.clone().map(Arc::new);
    let state = Arc::new(
        AppState::with_router(config.clone(), pipeline, upstreams, Some(smart), health)
            .with_storage(state_storage.clone())
            .with_custom_upstreams(custom_upstreams)
            .with_model_db(model_db.clone())
            .with_data_dir(paths.data_dir.clone()),
    );

    autorouter_server::model_db::trigger_scraping_if_needed(&state, &config, &paths.data_dir);

    // Bound in-memory (and SQLite) session growth for long-running
    // headless gateways. The JoinHandle is intentionally detached —
    // the task dies with the runtime on process exit.
    let _session_pruner = autorouter_server::start_session_pruner_default(state.sessions.clone());

    // The headless binary owns the same GatewaySupervisor as the desktop,
    // so PATCH /ui/settings can hot-rebind the listener without a restart.
    let supervisor = autorouter_server::GatewaySupervisor::new();
    let ui_state = autorouter_server::ui::UiState {
        config: Arc::new(parking_lot::RwLock::new(config.clone())),
        start_time: Arc::new(parking_lot::RwLock::new(chrono::Utc::now())),
        log_lines: log_buffer,
        config_path: Arc::new(parking_lot::RwLock::new(Some(
            paths.config_dir.join("config.toml"),
        ))),
        storage: Arc::new(parking_lot::RwLock::new(state_storage.clone())),
        secret_store: Arc::new(parking_lot::RwLock::new(Some(secret_store.clone()))),
        supervisor: Some(supervisor.clone()),
    };
    let initial_state = autorouter_server::RouterBuildState {
        bind: config.server.bind.clone(),
        enable_cors: config.server.cors_enabled(),
        max_body_bytes: config.server.max_body_bytes,
        request_timeout_seconds: config.server.request_timeout_seconds,
        stream_idle_timeout_seconds: config.server.stream_idle_timeout_seconds,
    };
    let ui_for_supervisor = ui_state.clone();
    let state_for_supervisor = (*state).clone();
    let supervisor_for_supervisor = supervisor.clone();
    let initial_addr = supervisor
        .clone()
        .start_with_state(
            autorouter_server::build_router_with_ui(
                state_for_supervisor,
                ui_for_supervisor,
                initial_state.enable_cors,
                Some(supervisor_for_supervisor),
            ),
            initial_state.clone(),
        )
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    tracing::info!(addr = %initial_addr, "AutoRouter listening");

    record_request("startup", "startup", "init");

    // If interactive and not --headless, run the TUI on the main thread.
    // Otherwise fall back to standard headless logging mode.
    if !headless && std::io::stdout().is_terminal() {
        if let Err(e) = tui::run_tui(state.clone(), ui_state).await {
            tracing::error!(error = %e, "TUI error");
        }
        supervisor.stop_graceful().await;
    } else {
        // The supervisor owns the listener and the axum task. We just
        // have to wait for Ctrl-C and then shut it down cleanly. While
        // we wait, `sync_router_state` (called from PATCH /ui/settings)
        // can swap the running router in place.
        tokio::select! {
            _ = signal::ctrl_c() => {
                supervisor.stop_graceful().await;
            }
        }
    }

    log_bridge.stop();
    if config.storage.backup_on_shutdown.unwrap_or(true) {
        // Documented scheme (manual.md): write a timestamped copy under
        // `<data_dir>/backups/`, then prune so only `backup_keep`
        // newest files remain. Do NOT call `rotate_backup` here —
        // that creates a different sibling-file scheme next to the
        // live DB and would leave the timestamped directory unbounded.
        if let Some(storage) = state.storage.as_ref() {
            let backup_dir = if config.storage.data_dir.is_empty() {
                db_path.parent().map(|p| p.join("backups"))
            } else {
                Some(std::path::PathBuf::from(&config.storage.data_dir).join("backups"))
            };
            if let Some(dir) = backup_dir.as_ref() {
                if let Err(e) = std::fs::create_dir_all(dir) {
                    tracing::warn!(error = %e, "failed to create backup dir");
                } else {
                    let db_name = db_path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("autorouter.db");
                    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
                    let backup_path = dir.join(format!("{db_name}.{stamp}"));
                    if let Err(e) = storage.shutdown(Some(&backup_path)) {
                        tracing::warn!(error = %e, "storage shutdown failed");
                    }
                    match autorouter_observability::prune_timestamped_backups(
                        dir,
                        db_name,
                        config.storage.backup_keep,
                    ) {
                        Ok(n) if n > 0 => {
                            tracing::info!(
                                pruned = n,
                                keep = config.storage.backup_keep,
                                "pruned old backups"
                            );
                        }
                        Ok(_) => {}
                        Err(e) => {
                            tracing::warn!(error = %e, "backup prune failed");
                        }
                    }
                }
            }
        }
    }
    tracing::info!("AutoRouter shut down cleanly");
    Ok(())
}
