//! Static-asset serving for the dashboard UI.
//!
//! When the AUTOROUTER_SERVE_UI=1 environment variable is set, the
//! gateway serves the contents of the `ui/dist/` directory on the
//! catch-all route so a real browser can drive the dashboard at
//! `http://127.0.0.1:4073/`. This is opt-in to keep the headless
//! binary minimal by default; the desktop binary uses its own
//! embedded webview and does not need this fallback.

use std::path::PathBuf;

use axum::http::Request;
use axum::response::{IntoResponse, Response};
use tower::Layer;
use tower_http::services::{ServeDir, ServeFile};

const ENV_VAR: &str = "AUTOROUTER_SERVE_UI";

/// Simple auth-checking layer that wraps any service. Injects an
/// auth check before every request to the static UI so the
/// dashboard SPA is protected when `require_auth=true`.
#[derive(Clone)]
pub struct UiAuthLayer {
    pub state: crate::AppState,
}

impl<S> Layer<S> for UiAuthLayer {
    type Service = UiAuthService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        UiAuthService {
            inner,
            state: self.state.clone(),
        }
    }
}

#[derive(Clone)]
pub struct UiAuthService<S> {
    inner: S,
    state: crate::AppState,
}

impl<S, ReqBody> tower::Service<Request<ReqBody>> for UiAuthService<S>
where
    S: tower::Service<Request<ReqBody>, Response = Response, Error = std::convert::Infallible>
        + Clone
        + Send
        + 'static,
    S::Future: Send,
    ReqBody: Send + 'static,
{
    type Response = Response;
    type Error = S::Error;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<ReqBody>) -> Self::Future {
        let state = self.state.clone();
        let mut inner = self.inner.clone();
        Box::pin(async move {
            let requires_auth;
            let expected_token: Option<String>;
            {
                let cfg = state.config.read();
                requires_auth = cfg.server.require_auth.unwrap_or(false);
                expected_token = cfg.server.auth_token.clone();
            }
            if requires_auth {
                // Mirror `routes::maybe_authorize`: when auth is required but
                // no token is configured, reject rather than silently allow —
                // a misconfigured gateway must not expose the dashboard SPA.
                let token = expected_token.as_deref();
                if token.is_none() {
                    return Ok((
                        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                        "auth required but no token configured",
                    )
                        .into_response());
                }
                let provided = req
                    .headers()
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.strip_prefix("Bearer "));
                if !crate::routes::ct_str_eq(provided.map(str::trim), token.map(|t| t.trim())) {
                    return Ok((
                        axum::http::StatusCode::UNAUTHORIZED,
                        "invalid or missing bearer token",
                    )
                        .into_response());
                }
            }
            inner.call(req).await
        })
    }
}

/// Returns the path to the UI dist directory if static-asset
/// serving is enabled and the directory exists. Resolves relative
/// to CARGO_MANIFEST_DIR at compile time so the dev loop picks up
/// the freshly built UI from `ui/dist`.
pub fn resolve_dist_dir() -> Option<PathBuf> {
    if std::env::var(ENV_VAR).ok().as_deref() != Some("1") {
        return None;
    }
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let candidates = [
        manifest_dir.join("..").join("..").join("ui").join("dist"),
        manifest_dir.join("..").join("ui").join("dist"),
        manifest_dir.join("ui").join("dist"),
    ];
    for p in &candidates {
        if p.is_dir() {
            return Some(p.clone());
        }
    }
    None
}

/// Build a router that serves the UI dist directory and falls
/// back to `index.html` for SPA routes. Used as the gateway's
/// catch-all when AUTOROUTER_SERVE_UI=1 is set.
///
/// The `state` parameter is used to enforce bearer auth on the
/// static files when `require_auth=true`, closing the gap where
/// the SPA assets (JS/CSS/HTML) were served without an auth check
/// while API routes were protected.
pub fn build_ui_fallback(dist_dir: PathBuf, state: crate::AppState) -> axum::Router {
    let serve = ServeDir::new(&dist_dir)
        .append_index_html_on_directories(true)
        .fallback(ServeFile::new(dist_dir.join("index.html")));
    axum::Router::new()
        .fallback_service(serve)
        .layer(UiAuthLayer { state })
}
