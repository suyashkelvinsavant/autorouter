//! End-to-end test for the analytics pipeline:
//!
//!   1. `MockUpstream::send` produces a realistic `usage` block and
//!      simulates non-zero latency so the dashboard has something
//!      meaningful to render when the gateway has no real upstream
//!      configured.
//!   2. `record_storage_event` (called from `call_upstream`) pulls
//!      the `Usage` from the upstream response and writes the token
//!      counts to the SQLite `provider_events` table.
//!   3. `GET /ui/analytics` aggregates those rows and reports the
//!      summed totals, the p50 / p95 latency, and the per-provider /
//!      per-model breakdowns.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::json;
use tower::ServiceExt;

use autorouter_core::ProviderKind;
use autorouter_server::ui::{UiAppState, UiState};
use autorouter_server::{
    AppState, MockUpstream, StorageHandle, TranslationPipeline, UpstreamClient,
};
use autorouter_translate::{
    AnthropicAdapter, GeminiAdapter, OpenAiChatAdapter, OpenAiResponsesAdapter,
};

fn build_state_with_storage(storage: Arc<StorageHandle>) -> AppState {
    let pipeline = TranslationPipeline::new()
        .register(Arc::new(OpenAiChatAdapter::new()))
        .register(Arc::new(OpenAiResponsesAdapter::new()))
        .register(Arc::new(AnthropicAdapter::new()))
        .register(Arc::new(GeminiAdapter::new()));
    let mut upstreams: HashMap<ProviderKind, Arc<dyn UpstreamClient>> = HashMap::new();
    upstreams.insert(
        ProviderKind::OpenAI,
        Arc::new(MockUpstream::new(ProviderKind::OpenAI)),
    );
    upstreams.insert(
        ProviderKind::Anthropic,
        Arc::new(MockUpstream::new(ProviderKind::Anthropic)),
    );
    upstreams.insert(
        ProviderKind::Gemini,
        Arc::new(MockUpstream::new(ProviderKind::Gemini)),
    );
    AppState::new(autorouter_config::AppConfig::default(), pipeline, upstreams)
        .with_storage(Some(storage))
}

/// Build a router with the UI sub-router merged so `/ui/*` routes
/// (analytics, events) are reachable. The UI state points at the
/// same storage handle the AppState uses, so events recorded by
/// `call_upstream` are visible to `get_analytics`.
fn build_full_router(storage: Arc<StorageHandle>) -> Router {
    let app_state = build_state_with_storage(storage.clone());
    let ui = UiState {
        config: Arc::new(parking_lot::RwLock::new(
            autorouter_config::AppConfig::default(),
        )),
        start_time: Arc::new(parking_lot::RwLock::new(chrono::Utc::now())),
        log_lines: Arc::new(parking_lot::RwLock::new(Vec::new())),
        config_path: Arc::new(parking_lot::RwLock::new(None)),
        storage: Arc::new(parking_lot::RwLock::new(Some(storage))),
        secret_store: Arc::new(parking_lot::RwLock::new(None)),
        supervisor: None,
    };
    let router = autorouter_server::build_router(app_state.clone());
    autorouter_server::ui::merge(
        router,
        UiAppState {
            ui,
            app: app_state,
            supervisor: None,
        },
    )
}

fn tempdir() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let base = std::env::temp_dir();
    let unique = format!(
        "autorouter-analytics-{}-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
    );
    let dir = base.join(unique);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn read_json(response: axum::response::Response) -> (StatusCode, serde_json::Value) {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, value)
}

fn chat_request(model: &str, content: &str) -> Request<Body> {
    let body = json!({
        "model": model,
        "messages": [{ "role": "user", "content": content }],
        "max_tokens": 32,
        "stream": false,
    });
    Request::builder()
        .method("POST")
        .uri("/openai/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

#[tokio::test]
async fn mock_upstream_default_response_includes_usage() {
    // The default mock response (when no body was configured) must
    // include a usage block so the analytics pipeline has token
    // counts to render even with no real upstream.
    let mock = MockUpstream::new(ProviderKind::OpenAI);
    let body = json!({
        "model": "gpt-4o-mini",
        "messages": [
            { "role": "system", "content": "You are a helpful assistant." },
            { "role": "user",   "content": "Tell me a short joke about programming." },
        ],
    });
    let resp = mock.send(&body).await.expect("send");
    assert_eq!(resp.status, 200);
    // The OpenAI Chat adapter decodes `usage.prompt_tokens` /
    // `usage.completion_tokens` into `Usage.tokens.input` / `output`.
    assert!(resp.response.usage.tokens.input.unwrap_or(0) > 0);
    assert!(resp.response.usage.tokens.output.unwrap_or(0) > 0);
    let total = resp.response.usage.total_tokens();
    assert!(total > 0, "expected total_tokens > 0, got {total}");
    // The raw envelope carries the canonical OpenAI shape so the
    // analytics path can introspect it directly.
    let raw_total = resp
        .raw
        .get("usage")
        .and_then(|u| u.get("total_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    assert!(raw_total > 0, "expected raw usage.total_tokens > 0");
}

#[tokio::test]
async fn mock_upstream_usage_scales_with_prompt_length() {
    // A longer prompt should produce more input tokens than a
    // short one. The estimator uses a ~4 chars/token heuristic so
    // doubling the prompt should at least roughly double the count.
    let mock = MockUpstream::new(ProviderKind::OpenAI);
    let short = mock
        .send(&json!({
            "model": "gpt-4o-mini",
            "messages": [{ "role": "user", "content": "hi" }],
        }))
        .await
        .expect("short");
    let long = mock
        .send(&json!({
            "model": "gpt-4o-mini",
            "messages": [{
                "role": "user",
                "content": "Please write a comprehensive essay about the history of computing, \
                             covering the abacus, Babbage's difference engine, ENIAC, the transistor, \
                             integrated circuits, the personal computer revolution, the rise of the \
                             internet, the dot-com boom, mobile computing, cloud computing, and \
                             the current state of artificial intelligence research today."
            }],
        }))
        .await
        .expect("long");
    let short_in = short.response.usage.tokens.input.unwrap_or(0);
    let long_in = long.response.usage.tokens.input.unwrap_or(0);
    assert!(
        long_in > short_in,
        "long prompt should have more input tokens than short: short={short_in} long={long_in}"
    );
    assert!(long_in >= 50, "long prompt should have plenty of tokens");
}

#[tokio::test]
async fn mock_upstream_simulates_latency_above_zero() {
    // The mock's send() path must sleep for 50-300ms so the
    // analytics pipeline records non-zero latency_ms. We assert a
    // loose lower bound here; the upper bound is intentionally not
    // tested so the test stays fast.
    let mock = MockUpstream::new(ProviderKind::OpenAI);
    let start = std::time::Instant::now();
    let _ = mock
        .send(&json!({
            "model": "gpt-4o-mini",
            "messages": [{ "role": "user", "content": "ping" }],
        }))
        .await
        .expect("send");
    let elapsed = start.elapsed().as_millis() as u64;
    assert!(
        elapsed >= 40,
        "expected mock to sleep at least ~50ms; elapsed={elapsed}ms"
    );
}

#[tokio::test]
async fn gateway_records_token_columns_in_provider_events() {
    // Sending a real request through the gateway must persist the
    // token counts from the mock's usage block into the SQLite
    // provider_events row (schema migration v4 added the columns;
    // this test guards against a regression where the columns are
    // present but never populated).
    let tmp = tempdir();
    let path = tmp.join("autorouter.db");
    let storage = Arc::new(StorageHandle::open(path).expect("open storage"));
    let app = build_full_router(storage.clone());

    let body = json!({
        "model": "gpt-4o-mini",
        "messages": [
            { "role": "user", "content": "tell me about Rust ownership and borrowing" }
        ],
        "max_tokens": 32,
    });
    let (status, _resp) = read_json(
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/openai/v1/chat/completions")
                .header("content-type", "application/json")
                .header("x-autorouter-source", "openai")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let events = storage
        .recent_provider_events("openai", 10)
        .expect("read events");
    let last = events.first().expect("at least one event");
    assert!(
        last.input_tokens > 0,
        "expected input_tokens > 0, got {}",
        last.input_tokens
    );
    assert!(
        last.output_tokens > 0,
        "expected output_tokens > 0, got {}",
        last.output_tokens
    );
    assert!(
        last.latency_ms > 0,
        "expected latency_ms > 0, got {}",
        last.latency_ms
    );
}

#[tokio::test]
async fn get_analytics_aggregates_token_counts_from_events() {
    // After two requests the /ui/analytics endpoint must report
    // total_input_tokens / total_output_tokens that sum the
    // per-event columns, not hard-coded zeros.
    let tmp = tempdir();
    let path = tmp.join("autorouter.db");
    let storage = Arc::new(StorageHandle::open(path).expect("open storage"));
    let app = build_full_router(storage.clone());

    for prompt in &["hello", "goodbye"] {
        let (status, _) = read_json(
            app.clone()
                .oneshot(chat_request("gpt-4o-mini", prompt))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    let (status, body) = read_json(
        app.oneshot(
            Request::builder()
                .method("GET")
                .uri("/ui/analytics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    assert_eq!(
        body.get("total_requests")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        2,
        "expected two requests aggregated, body={body}"
    );
    let in_t = body
        .get("total_input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let out_t = body
        .get("total_output_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    assert!(in_t > 0, "expected total_input_tokens > 0, got {in_t}");
    assert!(out_t > 0, "expected total_output_tokens > 0, got {out_t}");
    assert!(
        body.get("latency_recorded")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        "expected latency_recorded = true when events have non-zero latency"
    );
    let p50 = body
        .get("p50_latency_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let p95 = body
        .get("p95_latency_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    assert!(p50 > 0, "expected p50_latency_ms > 0, got {p50}");
    assert!(p95 > 0, "expected p95_latency_ms > 0, got {p95}");
    let by_provider = body
        .get("by_provider")
        .and_then(|v| v.as_array())
        .expect("by_provider");
    let openai = by_provider
        .iter()
        .find(|p| p.get("provider").and_then(|v| v.as_str()) == Some("openai"))
        .expect("openai bucket");
    assert!(
        openai
            .get("input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            > 0,
        "by_provider.openai.input_tokens should be > 0"
    );
}

#[tokio::test]
async fn get_events_includes_token_fields_per_event() {
    // The /ui/events endpoint must surface the token counts so the
    // frontend can build its own per-event views (e.g. a "PONG"
    // request detail) without a separate analytics round-trip.
    let tmp = tempdir();
    let path = tmp.join("autorouter.db");
    let storage = Arc::new(StorageHandle::open(path).expect("open storage"));
    let app = build_full_router(storage);

    let (status, _) = read_json(
        app.clone()
            .oneshot(chat_request("gpt-4o-mini", "ping"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = read_json(
        app.oneshot(
            Request::builder()
                .method("GET")
                .uri("/ui/events?provider=openai&limit=10")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let events = body
        .get("events")
        .and_then(|v| v.as_array())
        .expect("events");
    assert!(!events.is_empty());
    let first = &events[0];
    assert!(first.get("input_tokens").is_some(), "missing input_tokens");
    assert!(
        first.get("output_tokens").is_some(),
        "missing output_tokens"
    );
    assert!(
        first
            .get("input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            > 0,
        "input_tokens should be > 0 on the row"
    );
    assert!(
        first
            .get("latency_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            > 0,
        "latency_ms should be > 0 on the row"
    );
}

#[tokio::test]
async fn analytics_empty_when_no_events() {
    // A freshly-opened storage with no requests should report
    // total_requests == 0, zero tokens, and latency_recorded ==
    // false so the dashboard can render the empty state cleanly.
    let tmp = tempdir();
    let path = tmp.join("autorouter.db");
    let storage = Arc::new(StorageHandle::open(path).expect("open storage"));
    let app = build_full_router(storage);
    let (status, body) = read_json(
        app.oneshot(
            Request::builder()
                .method("GET")
                .uri("/ui/analytics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.get("total_requests").and_then(|v| v.as_u64()), Some(0));
    assert_eq!(
        body.get("latency_recorded").and_then(|v| v.as_bool()),
        Some(false)
    );
    assert_eq!(
        body.get("events_examined").and_then(|v| v.as_u64()),
        Some(0)
    );
}
