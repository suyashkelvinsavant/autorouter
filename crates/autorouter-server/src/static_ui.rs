//! Static-asset serving for the dashboard UI.
//!
//! When the AUTOROUTER_SERVE_UI=1 environment variable is set, the
//! gateway serves the contents of the `ui/dist/` directory on the
//! catch-all route so a real browser can drive the dashboard at
//! `http://127.0.0.1:4073/`. This is opt-in to keep the headless
//! binary minimal by default; the desktop binary uses its own
//! embedded webview and does not need this fallback.

use std::path::PathBuf;

const ENV_VAR: &str = "AUTOROUTER_SERVE_UI";

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
pub fn build_ui_fallback(dist_dir: PathBuf) -> axum::Router {
    // `ServeDir::append_index_html_on_directories` makes directories
    // serve `index.html`; `fallback` returns `index.html` for any
    // non-asset path so client-side routing works for routes like
    // `/dashboard`, `/settings`, etc.
    let serve = tower_http::services::ServeDir::new(&dist_dir)
        .append_index_html_on_directories(true)
        .fallback(tower_http::services::ServeFile::new(
            dist_dir.join("index.html"),
        ));
    axum::Router::new().fallback_service(serve)
}
