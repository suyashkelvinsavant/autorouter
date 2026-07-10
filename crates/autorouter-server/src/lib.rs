#![deny(unused_crate_dependencies)]
//! autorouter-server
//!
//! Local HTTP gateway that emulates the OpenAI, Anthropic, and Gemini
//! APIs and forwards traffic to a real upstream.
//!
//! See `docs/architecture.md` for the startup contract and boot order.
//! Both binaries (headless and desktop) must follow the same sequence:
//! resolve paths → load config → storage → settings overlay → logging →
//! pipeline → health → router → secret store → upstreams → supervisor → serve.

pub mod error;
pub mod log_bridge;
pub mod model_db;
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
pub use model_db::{
    check_and_save_scraped_models, find_similar_model_and_provider, run_scraping_job,
    should_update_scraped_models, ModelDb, ScrapedModel,
};
pub use router::{build_router, build_router_with_cors, build_router_with_ui};
pub use session::{
    run_session_pruner, start_session_pruner, start_session_pruner_default, Session,
    SessionRegistry, DEFAULT_SESSION_CAP, DEFAULT_SESSION_MAX_AGE, DEFAULT_SESSION_PRUNE_INTERVAL,
};
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
