//! Tests for the gateway supervisor: the hot-rebind layer that
//! makes "Save server" in the Settings page actually move the
//! listening socket. See `src/supervisor.rs` for the
//! implementation.

use std::time::Duration;

use axum::routing::get;
use axum::Router;

use autorouter_server::{GatewaySupervisor, RebindOutcome, RouterBuildState};

async fn hello() -> &'static str {
    "hello"
}

fn build_router() -> Router {
    Router::new().route("/healthz", get(hello))
}

fn pick_port() -> u16 {
    // Bind to port 0 to let the kernel pick a free ephemeral port.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind 0");
    let port = listener.local_addr().expect("local_addr").port();
    drop(listener);
    port
}

/// Convenience: build a `RouterBuildState` with default body /
/// timeout values so the tests that only care about CORS or bind
/// don't have to spell out the full struct every time. Defaults
/// match `ServerConfig::default()` so production and test state
/// agree.
fn state(bind: &str, enable_cors: bool) -> RouterBuildState {
    RouterBuildState {
        bind: bind.to_string(),
        enable_cors,
        max_body_bytes: 16 * 1024 * 1024,
        request_timeout_seconds: 300,
        stream_idle_timeout_seconds: 600,
    }
}

#[tokio::test]
async fn start_binds_and_reports_addr() {
    let port = pick_port();
    let bind = format!("127.0.0.1:{port}");
    let supervisor = GatewaySupervisor::new();
    let addr = supervisor
        .clone()
        .start(build_router(), &bind)
        .await
        .expect("start");
    assert_eq!(addr.port(), port);
    assert_eq!(supervisor.current_bind().as_deref(), Some(bind.as_str()));
    assert!(supervisor.is_running());
    supervisor.stop();
    assert!(!supervisor.is_running());
}

#[tokio::test]
async fn start_rejects_invalid_bind_string() {
    let supervisor = GatewaySupervisor::new();
    let err = supervisor
        .clone()
        .start(build_router(), "not a socket addr")
        .await
        .expect_err("invalid bind should fail");
    assert!(err.contains("invalid bind"), "error was {err}");
    assert!(!supervisor.is_running());
}

#[tokio::test]
async fn rebind_if_needed_noops_when_already_on_target() {
    let port = pick_port();
    let bind = format!("127.0.0.1:{port}");
    let supervisor = GatewaySupervisor::new();
    supervisor
        .clone()
        .start(build_router(), &bind)
        .await
        .expect("start");
    let outcome = supervisor
        .clone()
        .rebind_if_needed(&bind, || async { build_router() })
        .await
        .expect("rebind_if_needed");
    assert_eq!(outcome, RebindOutcome::AlreadyOnTarget);
    assert_eq!(supervisor.current_bind().as_deref(), Some(bind.as_str()));
    supervisor.stop();
}

#[tokio::test]
async fn rebind_moves_the_listening_socket() {
    let port_a = pick_port();
    let port_b = pick_port();
    let bind_a = format!("127.0.0.1:{port_a}");
    let bind_b = format!("127.0.0.1:{port_b}");
    let supervisor = GatewaySupervisor::new();
    supervisor
        .clone()
        .start(build_router(), &bind_a)
        .await
        .expect("start");
    assert_eq!(supervisor.current_bind().as_deref(), Some(bind_a.as_str()));

    // Hit the old listener to confirm it is alive.
    let old = reqwest::get(format!("http://{bind_a}/healthz"))
        .await
        .expect("reqwest on old bind");
    assert_eq!(old.status(), 200);
    assert_eq!(old.text().await.unwrap(), "hello");

    // Rebind to the new port.
    let outcome = supervisor
        .clone()
        .rebind_if_needed(&bind_b, || async { build_router() })
        .await
        .expect("rebind");
    assert_eq!(outcome, RebindOutcome::Rebound);
    assert_eq!(supervisor.current_bind().as_deref(), Some(bind_b.as_str()));

    // The new port now serves the same handler.
    let new = reqwest::get(format!("http://{bind_b}/healthz"))
        .await
        .expect("reqwest on new bind");
    assert_eq!(new.status(), 200);
    assert_eq!(new.text().await.unwrap(), "hello");

    supervisor.stop();
}

#[tokio::test]
async fn rebind_rejects_malformed_target_without_tearing_down() {
    let port = pick_port();
    let bind = format!("127.0.0.1:{port}");
    let supervisor = GatewaySupervisor::new();
    supervisor
        .clone()
        .start(build_router(), &bind)
        .await
        .expect("start");

    let err = supervisor
        .clone()
        .rebind_if_needed("not a socket addr", || async { build_router() })
        .await
        .expect_err("malformed target should fail");
    assert!(err.contains("invalid bind"), "error was {err}");
    // The original listener should still be alive.
    assert_eq!(supervisor.current_bind().as_deref(), Some(bind.as_str()));
    let probe = reqwest::get(format!("http://{bind}/healthz"))
        .await
        .expect("reqwest on original bind");
    assert_eq!(probe.status(), 200);
    supervisor.stop();
}

#[tokio::test]
async fn start_from_idle_binds_and_serves() {
    let port = pick_port();
    let bind = format!("127.0.0.1:{port}");
    let supervisor = GatewaySupervisor::new();
    assert!(!supervisor.is_running());
    supervisor
        .clone()
        .start(build_router(), &bind)
        .await
        .expect("start from idle");
    assert!(supervisor.is_running());
    // Give the axum task a moment to start accepting connections.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let probe = reqwest::get(format!("http://{bind}/healthz"))
        .await
        .expect("reqwest on fresh bind");
    assert_eq!(probe.status(), 200);
    supervisor.stop();
}

#[tokio::test]
async fn sync_router_state_noops_when_state_unchanged() {
    let port = pick_port();
    let bind = format!("127.0.0.1:{port}");
    let supervisor = GatewaySupervisor::new();
    supervisor
        .clone()
        .start_with_state(build_router(), state(&bind, false))
        .await
        .expect("start_with_state");
    let outcome = supervisor
        .clone()
        .sync_router_state(state(&bind, false), || async { build_router() })
        .await
        .expect("sync_router_state");
    assert_eq!(outcome, RebindOutcome::AlreadyOnTarget);
    assert_eq!(supervisor.current_state(), Some(state(&bind, false)));
    supervisor.stop();
}

#[tokio::test]
async fn sync_router_state_rebinds_on_cors_toggle() {
    // Regression for gap #4: previously CORS only flipped on a
    // process restart because `rebind_if_needed` compared bind
    // strings only. With `RouterBuildState` a CORS toggle is now
    // a real rebind event.
    let port = pick_port();
    let bind = format!("127.0.0.1:{port}");
    let supervisor = GatewaySupervisor::new();
    supervisor
        .clone()
        .start_with_state(build_router(), state(&bind, false))
        .await
        .expect("start_with_state (cors off)");
    assert_eq!(supervisor.current_state(), Some(state(&bind, false)));
    // Toggle CORS on; bind unchanged.
    let outcome = supervisor
        .clone()
        .sync_router_state(state(&bind, true), || async { build_router() })
        .await
        .expect("sync_router_state (cors on)");
    assert_eq!(outcome, RebindOutcome::Rebound);
    assert_eq!(supervisor.current_state(), Some(state(&bind, true)));
    // And back off again — a second toggle must also flip state.
    let outcome = supervisor
        .clone()
        .sync_router_state(state(&bind, false), || async { build_router() })
        .await
        .expect("sync_router_state (cors off)");
    assert_eq!(outcome, RebindOutcome::Rebound);
    assert_eq!(supervisor.current_state(), Some(state(&bind, false)));
    supervisor.stop();
}

#[tokio::test]
async fn sync_router_state_rebinds_on_body_limit_change() {
    // Companion to gap #4: `max_body_bytes` is baked into the
    // running router as a `RequestBodyLimitLayer`. Toggling it
    // without a rebind silently keeps the old limit, so the
    // supervisor must treat a body-limit change as a real
    // rebind event.
    let port = pick_port();
    let bind = format!("127.0.0.1:{port}");
    let supervisor = GatewaySupervisor::new();
    supervisor
        .clone()
        .start_with_state(build_router(), state(&bind, false))
        .await
        .expect("start_with_state");
    let bigger = RouterBuildState {
        max_body_bytes: 64 * 1024 * 1024,
        ..state(&bind, false)
    };
    let outcome = supervisor
        .clone()
        .sync_router_state(bigger.clone(), || async { build_router() })
        .await
        .expect("sync_router_state (bigger body limit)");
    assert_eq!(outcome, RebindOutcome::Rebound);
    assert_eq!(supervisor.current_state(), Some(bigger));
    supervisor.stop();
}

#[tokio::test]
async fn sync_router_state_rebinds_on_timeout_change() {
    // Companion to gap #4: `request_timeout_seconds` is baked
    // into the running router as a state parameter on the
    // timeout middleware. A no-op supervisor would let the
    // gateway keep the old timeout after the operator moved
    // the slider in the Settings page.
    let port = pick_port();
    let bind = format!("127.0.0.1:{port}");
    let supervisor = GatewaySupervisor::new();
    supervisor
        .clone()
        .start_with_state(build_router(), state(&bind, false))
        .await
        .expect("start_with_state");
    let slower = RouterBuildState {
        request_timeout_seconds: 900,
        ..state(&bind, false)
    };
    let outcome = supervisor
        .clone()
        .sync_router_state(slower.clone(), || async { build_router() })
        .await
        .expect("sync_router_state (slower timeout)");
    assert_eq!(outcome, RebindOutcome::Rebound);
    assert_eq!(supervisor.current_state(), Some(slower));
    supervisor.stop();
}

#[tokio::test]
async fn stop_graceful_returns_when_idle() {
    // Regression guard: `stop_graceful` must be safe to call on
    // an idle supervisor (no panics, no hangs).
    let supervisor = GatewaySupervisor::new();
    supervisor.stop_graceful().await;
    assert!(!supervisor.is_running());
}

#[tokio::test]
async fn stop_graceful_drains_then_returns() {
    // Regression guard: when there is a running gateway, the
    // graceful variant must signal shutdown and complete within
    // the grace window instead of aborting immediately.
    let port = pick_port();
    let bind = format!("127.0.0.1:{port}");
    let supervisor = GatewaySupervisor::new();
    supervisor
        .clone()
        .start_with_state(build_router(), state(&bind, false))
        .await
        .expect("start_with_state");
    // Give the axum task a beat to start accepting connections,
    // otherwise `Notify::notified()` may fire before the task
    // is fully spun up and the join future stalls until the
    // grace window expires.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let started = std::time::Instant::now();
    supervisor.stop_graceful().await;
    let elapsed = started.elapsed();
    // The grace window is 1s; even with no in-flight requests
    // we should finish well under that.
    assert!(
        elapsed < std::time::Duration::from_millis(800),
        "stop_graceful took {elapsed:?}"
    );
    assert!(!supervisor.is_running());
}
