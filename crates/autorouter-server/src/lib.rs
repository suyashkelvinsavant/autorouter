#![deny(unused_crate_dependencies)]
//! autorouter-server
//!
//! Local HTTP gateway that emulates the OpenAI, Anthropic, and Gemini
//! APIs and forwards traffic to a real upstream.
//!
//! # Startup contract (canonical boot order)
//!
//! Both binaries (`autorouter-app` headless and `autorouter-desktop`)
//! MUST execute these steps in order. Deviations cause hard-to-debug
//! bugs (e.g. settings overlay before storage is open, or upstream map
//! built with stale config).
//!
//! 1. **Resolve paths** — `ProjectPaths::resolve()` (fall back to
//!    `ProjectPaths::under_root(".")`).
//! 2. **Load config** — `ConfigLoader::from_standard_chain(&paths)`.
//! 3. **Validate storage** — `validate_storage(&config.storage)` so
//!    bad paths fail early.
//! 4. **Open SQLite storage** — `StorageHandle::open(db_path)`. Wrap
//!    errors as non-fatal warnings (gateway works without persistence).
//! 5. **Overlay runtime settings** — `get_setting("auth_token")`,
//!    `get_setting("default_provider")`, `get_setting("default_model")`
//!    from the storage handle opened in step 4.
//! 6. **Init log bridge** — create `Arc<RwLock<Vec<LogLine>>>`, start
//!    `LogBridge::start(...)`.
//! 7. **Init tracing** — `install_log_sink()` then
//!    `init_logging(...)`.
//! 8. **Build pipeline** — `TranslationPipeline::new()` + register
//!    all four adapters.
//! 9. **Build health tracker** — `HealthTracker::new()`. Shared
//!    between `SmartRouter` and `AppState`.
//! 10. **Build smart router** — `build_smart_router(&pipeline, &config, health)`.
//! 11. **Build secret store** — `build_secret_store(...)` from env
//!     or keychain default.
//! 12. **Build upstreams** — `build_upstreams(&config, secret_store)`.
//! 13. **Create AppState** — `AppState::with_router(config, pipeline,
//!     upstreams, smart_router, health).with_storage(storage).with_custom_upstreams(custom)`.
//! 14. **Create UiState** — wire config, start_time, log_lines,
//!     config_path, storage, secret_store into `UiState { ... }`.
//! 15. **Create GatewaySupervisor** — `GatewaySupervisor::new()`.
//! 16. **Build router** — `build_router_with_ui(state, ui_state,
//!     enable_cors, supervisor)`.
//! 17. **Start server** — `supervisor.start_with_state(router,
//!     RouterBuildState { bind, enable_cors, ... })`.
//! 18. **Await shutdown** — `signal::ctrl_c()` or Tauri run event.
//! 19. **Stop & backup** — `log_bridge.stop()`, `storage.shutdown(backup_path)`,
//!     `rotate_backup(...)` if `backup_on_shutdown`.

pub mod error;
pub mod log_bridge;
pub mod router;
pub mod routes;
pub mod session;
pub mod state;
pub mod static_ui;
pub mod storage;
pub mod supervisor;
pub mod ui;
pub mod upstream;

pub use error::{ServerError, ServerResult};
pub use log_bridge::LogBridge;
pub use router::{build_router, build_router_with_cors, build_router_with_ui};
pub use session::{start_session_pruner, Session, SessionRegistry};
pub use state::{build_smart_router, user_config_path, AppState};
pub use storage::StorageHandle;
pub use supervisor::{GatewaySupervisor, RebindOutcome, RouterBuildState};
pub use ui::{merge, LogLine, UiAppState, UiState};
pub use upstream::{
    build_upstreams, rebuild_upstreams, resolve_secret, HttpUpstream, HttpUpstreamConfig,
    MockUpstream, SharedUpstream, UpstreamClient, UpstreamSet,
};

// Re-export for tests and downstream binaries.
pub use autorouter_translate::TranslationPipeline;

// Referenced by integration tests via `tower::ServiceExt`.
#[cfg(test)]
use tower as _;
