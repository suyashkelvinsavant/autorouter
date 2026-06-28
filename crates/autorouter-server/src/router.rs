//! Axum router wiring.

use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use tower_http::cors::{Any, CorsLayer};
use tower_http::limit::RequestBodyLimitLayer;

use crate::routes;
use crate::state::AppState;
use crate::static_ui as static_ui_mod;
use crate::ui::{UiAppState, UiState};

/// Build the Axum router with the standard AutoRouter surface area.
pub fn build_router(state: AppState) -> Router {
    let enable_cors = state.config.read().server.cors_enabled();
    build_router_with_cors(state, enable_cors)
}

/// Same as [`build_router`] but with an explicit CORS override.
pub fn build_router_with_cors(state: AppState, enable_cors: bool) -> Router {
    build_router_inner(state, enable_cors, None, None)
}

/// Build the router and merge the dashboard UI sub-router.
///
/// `supervisor` is forwarded to the resulting `UiAppState` so the HTTP
/// `patch_settings` and `restart` handlers can hot-rebind the listener
/// when the bind changes. Pass `None` for the headless binary (which
/// cannot rebind and is expected to be restarted by the operator).
pub fn build_router_with_ui(
    state: AppState,
    ui: UiState,
    enable_cors: bool,
    supervisor: Option<crate::supervisor::GatewaySupervisor>,
) -> Router {
    build_router_inner(state, enable_cors, Some(ui), supervisor)
}

fn build_router_inner(
    state: AppState,
    enable_cors: bool,
    ui_state: Option<UiState>,
    supervisor: Option<crate::supervisor::GatewaySupervisor>,
) -> Router {
    let body_limit = RequestBodyLimitLayer::new(state.config.read().server.max_body_bytes);
    // The request_timeout_seconds deadline is applied INSIDE the
    // non-streaming call path (`call_upstream`) rather than as a
    // router-level middleware. A router-level middleware would also
    // wrap the SSE streaming handlers and kill active streams when
    // the deadline elapses, even though they have their own
    // per-chunk idle timeout (`stream_idle_timeout_seconds`) that
    // detects truly stalled streams without cutting off live ones.

    async fn metrics_handler(
        headers: axum::http::HeaderMap,
        axum::extract::State(state): axum::extract::State<AppState>,
    ) -> axum::response::Response {
        if let Err(e) = routes::maybe_authorize(&headers, &state) {
            return e.into_response();
        }
        match autorouter_observability::render_metrics() {
            Ok(s) => (
                axum::http::StatusCode::OK,
                [(
                    axum::http::header::CONTENT_TYPE,
                    "text/plain; version=0.0.4",
                )],
                s,
            )
                .into_response(),
            Err(_) => axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    }

    // Each protocol endpoint has a single POST handler that
    // dispatches to streaming or non-streaming internally based on
    // the request body.
    let router = if let Some(ui) = ui_state {
        let combined = UiAppState {
            ui: ui.clone(),
            app: state.clone(),
            supervisor: supervisor.clone(),
        };
        let gateway = Router::new()
            .route("/healthz", get(routes::health))
            .route("/v1/sessions", get(routes::list_sessions))
            .route("/v1/models", get(routes::list_models))
            .route(
                "/openai/v1/chat/completions",
                post(routes::openai_chat_completions),
            )
            .route("/openai/v1/responses", post(routes::openai_responses))
            .route("/v1/messages", post(routes::anthropic_messages))
            .route(
                "/v1beta/models/*path",
                post(routes::gemini_generate_content),
            )
            .route("/metrics", get(metrics_handler))
            .with_state(state);
        let ui_sub = crate::ui::build_sub_router(combined);
        gateway.merge(ui_sub)
    } else {
        Router::new()
            .route("/healthz", get(routes::health))
            .route("/v1/sessions", get(routes::list_sessions))
            .route("/v1/models", get(routes::list_models))
            .route(
                "/openai/v1/chat/completions",
                post(routes::openai_chat_completions),
            )
            .route("/openai/v1/responses", post(routes::openai_responses))
            .route("/v1/messages", post(routes::anthropic_messages))
            .route(
                "/v1beta/models/*path",
                post(routes::gemini_generate_content),
            )
            .route("/metrics", get(metrics_handler))
            .with_state(state)
    };

    let router = router.layer(body_limit);

    let router = if enable_cors {
        router.layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
    } else {
        router
    };

    if let Some(dist_dir) = static_ui_mod::resolve_dist_dir() {
        tracing::info!(path = %dist_dir.display(), "serving UI dist directory (AUTOROUTER_SERVE_UI=1)");
        router.merge(static_ui_mod::build_ui_fallback(dist_dir))
    } else {
        router
    }
}
