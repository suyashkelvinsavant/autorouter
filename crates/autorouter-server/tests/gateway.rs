//! End-to-end tests for the local gateway.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use autorouter_core::ProviderKind;
use autorouter_server::{AppState, MockUpstream, TranslationPipeline};
use autorouter_translate::{
    AnthropicAdapter, GeminiAdapter, OpenAiChatAdapter, OpenAiResponsesAdapter,
};

fn build_state() -> AppState {
    let pipeline = TranslationPipeline::new()
        .register(Arc::new(OpenAiChatAdapter::new()))
        .register(Arc::new(OpenAiResponsesAdapter::new()))
        .register(Arc::new(AnthropicAdapter::new()))
        .register(Arc::new(GeminiAdapter::new()));
    let mut upstreams: HashMap<ProviderKind, Arc<dyn autorouter_server::UpstreamClient>> =
        HashMap::new();
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
}

async fn read_body(response: axum::response::Response) -> (StatusCode, String) {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

#[tokio::test]
async fn health_endpoint_responds() {
    let app = autorouter_server::build_router(build_state());
    let (status, body) = read_body(
        app.oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let value: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["status"], "ok");
}

#[tokio::test]
async fn openai_chat_endpoint_passes_through_mock() {
    let app = autorouter_server::build_router(build_state());
    let body = json!({
        "model": "gpt-5",
        "messages": [{ "role": "user", "content": "hi" }],
        "stream": false,
    });
    let (status, body) = read_body(
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/openai/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let value: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["object"], "chat.completion");
    assert_eq!(value["choices"][0]["message"]["role"], "assistant");
}

#[tokio::test]
async fn anthropic_endpoint_accepts_anthropic_payload() {
    let app = autorouter_server::build_router(build_state());
    let body = json!({
        "model": "claude-sonnet-4-5",
        "max_tokens": 256,
        "system": "be brief",
        "messages": [{ "role": "user", "content": "hi" }],
    });
    let (status, body) = read_body(
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .header("x-autorouter-source", "anthropic")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let value: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["type"], "message");
    assert_eq!(value["role"], "assistant");
}

#[tokio::test]
async fn gemini_endpoint_accepts_gemini_payload() {
    let app = autorouter_server::build_router(build_state());
    let body = json!({
        "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }],
        "generationConfig": { "maxOutputTokens": 64 },
    });
    let (status, body) = read_body(
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1beta/models/gemini-2.5-pro:generateContent")
                .header("content-type", "application/json")
                .header("x-autorouter-source", "gemini")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let value: Value = serde_json::from_str(&body).unwrap();
    assert!(value["candidates"].is_array());
}

#[tokio::test]
async fn openai_responses_endpoint() {
    let app = autorouter_server::build_router(build_state());
    let body = json!({
        "model": "gpt-5",
        "input": "hi",
    });
    let (status, body) = read_body(
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/openai/v1/responses")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let value: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["object"], "response");
    assert!(value["output"].is_array());
}

#[tokio::test]
async fn invalid_json_returns_bad_request() {
    let app = autorouter_server::build_router(build_state());
    let (status, _body) = read_body(
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/openai/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from("{not valid json"))
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn sessions_endpoint_returns_registered_sessions() {
    let state = build_state();
    let _session = state
        .sessions
        .get_or_create(None, "openai", Some("claude-code".into()));
    let app = autorouter_server::build_router(state);
    let (status, body) = read_body(
        app.oneshot(
            Request::builder()
                .uri("/v1/sessions")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let value: Value = serde_json::from_str(&body).unwrap();
    let sessions = value["sessions"].as_array().unwrap();
    assert!(!sessions.is_empty());
    assert_eq!(sessions[0]["source_provider"], "openai");
    assert_eq!(sessions[0]["label"], "claude-code");
}

// --- Gateway metadata and health routes must run through
// `maybe_authorize()` when require_auth is on ---

fn build_state_with_auth(token: &str) -> AppState {
    let state = build_state();
    {
        let cfg = state.current_config().as_ref().clone();
        let mut cfg = cfg;
        cfg.server.require_auth = Some(true);
        cfg.server.auth_token = Some(token.to_string());
        state.replace_config(cfg);
    }
    state
}

#[tokio::test]
async fn gap4_healthz_requires_bearer_when_auth_enabled() {
    let state = build_state_with_auth("test-bearer-token");
    let app = autorouter_server::build_router(state);
    // No Authorization header → must be rejected.
    let (status, _body) = read_body(
        app.clone()
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "/healthz must demand bearer auth under require_auth"
    );
    // Wrong token → still rejected.
    let (status, _body) = read_body(
        app.clone()
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .header("authorization", "Bearer wrong-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // Correct token → accepted.
    let (status, body) = read_body(
        app.oneshot(
            Request::builder()
                .uri("/healthz")
                .header("authorization", "Bearer test-bearer-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body.contains("\"status\":\"ok\""));
}

#[tokio::test]
async fn gap4_metrics_requires_bearer_when_auth_enabled() {
    let state = build_state_with_auth("test-bearer-token");
    let app = autorouter_server::build_router(state);
    // No Authorization header → must be rejected.
    let (status, _body) = read_body(
        app.clone()
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "/metrics must demand bearer auth under require_auth"
    );
    // Wrong token → still rejected.
    let (status, _body) = read_body(
        app.clone()
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .header("authorization", "Bearer wrong-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // Correct token → accepted.
    let (status, _body) = read_body(
        app.oneshot(
            Request::builder()
                .uri("/metrics")
                .header("authorization", "Bearer test-bearer-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "/metrics should return 200 with valid bearer token"
    );
}

#[tokio::test]
async fn gap4_v1_sessions_requires_bearer_when_auth_enabled() {
    let state = build_state_with_auth("test-bearer-token");
    let _session = state
        .sessions
        .get_or_create(None, "openai", Some("claude-code".into()));
    let app = autorouter_server::build_router(state);
    let (status, _body) = read_body(
        app.clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/sessions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "/v1/sessions must demand bearer auth under require_auth"
    );
    let (status, _body) = read_body(
        app.oneshot(
            Request::builder()
                .uri("/v1/sessions")
                .header("authorization", "Bearer test-bearer-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn gap4_v1_models_requires_bearer_when_auth_enabled() {
    let state = build_state_with_auth("test-bearer-token");
    let app = autorouter_server::build_router(state);
    let (status, _body) = read_body(
        app.clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "/v1/models must demand bearer auth under require_auth"
    );
    let (status, body) = read_body(
        app.oneshot(
            Request::builder()
                .uri("/v1/models")
                .header("authorization", "Bearer test-bearer-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
}

#[tokio::test]
async fn gap4_metadata_routes_open_when_require_auth_disabled() {
    // Back-compat: with require_auth = false (the default) all
    // three routes must still respond without a token.
    let state = build_state();
    let app = autorouter_server::build_router(state);
    for path in ["/healthz", "/v1/sessions", "/v1/models"] {
        let (status, _body) = read_body(
            app.clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{path} must remain public when require_auth is false"
        );
    }
}

// --- sentinel model resolution -------------------------------------------------
//
// A client that has no opinion about which upstream model to use
// (opencode configured with `model: "autorouter"`) sends the sentinel
// id and lets the routing engine decide. The gateway must substitute
// the resolved default model before the request reaches any upstream,
// so the editor can switch models at runtime from the dashboard
// without restarting. These tests pin that end-to-end contract: the
// upstream must never observe "autorouter" as a model id, and the
// response must report the resolved model.

/// Build an AppState whose OpenAI upstream is a recorded `MockUpstream`
/// the test can inspect after the call. Returns the state and the mock.
fn build_state_with_recorded_openai() -> (AppState, Arc<MockUpstream>) {
    let pipeline = TranslationPipeline::new()
        .register(Arc::new(OpenAiChatAdapter::new()))
        .register(Arc::new(OpenAiResponsesAdapter::new()))
        .register(Arc::new(AnthropicAdapter::new()))
        .register(Arc::new(GeminiAdapter::new()));
    let openai = Arc::new(MockUpstream::new(ProviderKind::OpenAI));
    let mut upstreams: HashMap<ProviderKind, Arc<dyn autorouter_server::UpstreamClient>> =
        HashMap::new();
    upstreams.insert(ProviderKind::OpenAI, openai.clone());
    upstreams.insert(
        ProviderKind::Anthropic,
        Arc::new(MockUpstream::new(ProviderKind::Anthropic)),
    );
    upstreams.insert(
        ProviderKind::Gemini,
        Arc::new(MockUpstream::new(ProviderKind::Gemini)),
    );
    let mut config = autorouter_config::AppConfig::default();
    config.defaults.default_model = "gpt-5".to_string();
    config.defaults.default_provider = "openai".to_string();
    (AppState::new(config, pipeline, upstreams), openai)
}

#[tokio::test]
async fn sentinel_model_is_resolved_before_reaching_upstream() {
    let (state, openai) = build_state_with_recorded_openai();
    let app = autorouter_server::build_router(state);
    let body = json!({
        "model": "autorouter",
        "messages": [{ "role": "user", "content": "hi" }],
        "stream": false,
    });
    let (status, resp) = read_body(
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/openai/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {resp}");
    let v: Value = serde_json::from_str(&resp).unwrap();
    // The response must report the resolved default model, never the sentinel.
    assert_ne!(
        v["model"], "autorouter",
        "sentinel leaked into the response model"
    );
    assert_eq!(
        v["model"], "gpt-5",
        "should resolve to the configured default"
    );
    // The upstream body must carry the real model id.
    let recorded = openai.recorded();
    assert_eq!(recorded.len(), 1, "exactly one upstream call expected");
    assert_eq!(
        recorded[0]["model"], "gpt-5",
        "the upstream must never observe the sentinel model id"
    );
}

#[tokio::test]
async fn sentinel_model_picks_up_operator_chosen_default() {
    // Switching the routed model from the dashboard (PATCH /ui/settings
    // `default_model`) takes effect on the next sentinel request,
    // with no editor restart.
    let (state, openai) = build_state_with_recorded_openai();
    {
        let cfg = state.current_config().as_ref().clone();
        let mut cfg = cfg;
        cfg.defaults.default_model = "claude-sonnet-4-5".to_string();
        state.replace_config(cfg);
    }
    let app = autorouter_server::build_router(state);
    let body = json!({
        "model": "autorouter",
        "messages": [{ "role": "user", "content": "hi" }],
        "stream": false,
    });
    let (status, resp) = read_body(
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/openai/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {resp}");
    let v: Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(v["model"], "claude-sonnet-4-5");
    let recorded = openai.recorded();
    assert_eq!(recorded[0]["model"], "claude-sonnet-4-5");
}

#[tokio::test]
async fn explicit_model_is_passed_through_unchanged() {
    // Regression guard: a concrete model id must reach the upstream
    // untouched. The sentinel rewrite must not affect normal traffic.
    let (state, openai) = build_state_with_recorded_openai();
    let app = autorouter_server::build_router(state);
    let body = json!({
        "model": "gpt-4o",
        "messages": [{ "role": "user", "content": "hi" }],
        "stream": false,
    });
    let (status, resp) = read_body(
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/openai/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {resp}");
    let v: Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(v["model"], "gpt-4o");
    assert_eq!(openai.recorded()[0]["model"], "gpt-4o");
}

#[tokio::test]
async fn sentinel_model_streaming_resolves_upstream_body() {
    // opencode streams by default, so the streaming path must resolve
    // the sentinel too. `MockUpstream::send_streaming` records the
    // body before returning an empty stream.
    let (state, openai) = build_state_with_recorded_openai();
    let app = autorouter_server::build_router(state);
    let body = json!({
        "model": "autorouter",
        "messages": [{ "role": "user", "content": "hi" }],
        "stream": true,
    });
    let (status, _resp) = read_body(
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/openai/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "streaming sentinel request must succeed"
    );
    let recorded = openai.recorded();
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0]["model"], "gpt-5",
        "streaming upstream must see the resolved model, not the sentinel"
    );
}

#[tokio::test]
async fn sentinel_model_resolves_through_production_smart_router() {
    // The desktop/headless shells boot a SmartRouter built from
    // config (startup step 10). The other sentinel tests use the dev
    // IdentityRouter; this one proves the sentinel resolves end-to-end
    // through the exact router production runs.
    use autorouter_router::HealthTracker;
    let pipeline = TranslationPipeline::new()
        .register(Arc::new(OpenAiChatAdapter::new()))
        .register(Arc::new(OpenAiResponsesAdapter::new()))
        .register(Arc::new(AnthropicAdapter::new()))
        .register(Arc::new(GeminiAdapter::new()));
    let mut config = autorouter_config::AppConfig::default();
    config.defaults.default_model = "gpt-5".to_string();
    config.defaults.default_provider = "openai".to_string();
    let smart_router = autorouter_server::build_smart_router(
        &pipeline,
        &config,
        HealthTracker::new(),
        &autorouter_server::ModelDb::bundled_defaults(),
    );
    let router: Arc<dyn autorouter_router::Router> = smart_router;
    let openai = Arc::new(MockUpstream::new(ProviderKind::OpenAI));
    let mut upstreams: HashMap<ProviderKind, Arc<dyn autorouter_server::UpstreamClient>> =
        HashMap::new();
    upstreams.insert(ProviderKind::OpenAI, openai.clone());
    let state = AppState::with_router(
        config,
        pipeline,
        upstreams,
        Some(router),
        HealthTracker::new(),
    );
    let app = autorouter_server::build_router(state);
    let body = json!({
        "model": "autorouter",
        "messages": [{ "role": "user", "content": "hi" }],
        "stream": false,
    });
    let (status, resp) = read_body(
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/openai/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {resp}");
    let recorded = openai.recorded();
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0]["model"], "gpt-5",
        "SmartRouter must resolve the sentinel to the configured default model"
    );
}

#[tokio::test]
async fn sentinel_model_with_smart_router_and_no_default_returns_error() {
    // When using SmartRouter (production mode) with the sentinel model
    // but no default model configured, the gateway should return a clear
    // error instead of silently passing through the sentinel to upstream.
    use autorouter_router::HealthTracker;
    let pipeline = TranslationPipeline::new()
        .register(Arc::new(OpenAiChatAdapter::new()))
        .register(Arc::new(OpenAiResponsesAdapter::new()))
        .register(Arc::new(AnthropicAdapter::new()))
        .register(Arc::new(GeminiAdapter::new()));
    let mut config = autorouter_config::AppConfig::default();
    // Clear the default model to simulate a misconfigured production setup
    config.defaults.default_model = String::new();
    let smart_router = autorouter_server::build_smart_router(
        &pipeline,
        &config,
        HealthTracker::new(),
        &autorouter_server::ModelDb::bundled_defaults(),
    );
    let router: Arc<dyn autorouter_router::Router> = smart_router;
    let openai = Arc::new(MockUpstream::new(ProviderKind::OpenAI));
    let mut upstreams: HashMap<ProviderKind, Arc<dyn autorouter_server::UpstreamClient>> =
        HashMap::new();
    upstreams.insert(ProviderKind::OpenAI, openai.clone());
    let state = AppState::with_router(
        config,
        pipeline,
        upstreams,
        Some(router),
        HealthTracker::new(),
    );
    let app = autorouter_server::build_router(state);
    let body = json!({
        "model": "autorouter",
        "messages": [{ "role": "user", "content": "hi" }],
        "stream": false,
    });
    let (status, resp) = read_body(
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/openai/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "SmartRouter with no default and sentinel model should return 400"
    );
    let v: Value = serde_json::from_str(&resp).unwrap();
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no default model is configured"),
        "error message should explain the configuration issue"
    );
    // No upstream call should have been made
    assert_eq!(
        openai.recorded().len(),
        0,
        "request should fail before reaching upstream"
    );
}

// --- failover cascade tests -------------------------------------------------
//
// These tests pin the behaviour introduced alongside `call_upstream_with_failover`:
//   * 5xx on the primary triggers retry on other configured providers
//   * 4xx does NOT trigger retry (client error is returned immediately)
//   * when all same-model providers fail, the similar-model fallback is tried
//   * when nothing works, the original error reaches the client

/// Build a state with both OpenAI and Anthropic upstreams, each
/// independently controllable via `set_error_status`.
fn build_failover_state() -> (AppState, Arc<MockUpstream>, Arc<MockUpstream>) {
    let pipeline = TranslationPipeline::new()
        .register(Arc::new(OpenAiChatAdapter::new()))
        .register(Arc::new(OpenAiResponsesAdapter::new()))
        .register(Arc::new(AnthropicAdapter::new()))
        .register(Arc::new(GeminiAdapter::new()));
    let openai = Arc::new(MockUpstream::new(ProviderKind::OpenAI));
    let anthropic = Arc::new(MockUpstream::new(ProviderKind::Anthropic));
    let mut upstreams: HashMap<ProviderKind, Arc<dyn autorouter_server::UpstreamClient>> =
        HashMap::new();
    upstreams.insert(ProviderKind::OpenAI, openai.clone());
    upstreams.insert(ProviderKind::Anthropic, anthropic.clone());
    let mut config = autorouter_config::AppConfig::default();
    config.defaults.default_model = "gpt-5".to_string();
    config.defaults.default_provider = "openai".to_string();
    // Enable both providers with empty allowlists (all models allowed)
    // so find_other_providers_for_model considers both as failover targets.
    config.providers.openai = Some(autorouter_config::ProviderEntry::default());
    config.providers.anthropic = Some(autorouter_config::ProviderEntry::default());
    (
        AppState::new(config, pipeline, upstreams),
        openai,
        anthropic,
    )
}

#[tokio::test]
async fn failover_5xx_retries_on_other_provider_without_override() {
    let (state, openai, anthropic) = build_failover_state();
    // Make OpenAI return 503; Anthropic stays healthy (200).
    openai.set_error_status(Some(503));
    let app = autorouter_server::build_router(state);
    let body = json!({
        "model": "gpt-5",
        "messages": [{ "role": "user", "content": "hi" }],
        "stream": false,
    });
    let (status, _resp) = read_body(
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/openai/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    // The router sends to OpenAI (identity/default); OpenAI returns
    // 503 → failover finds another provider that serves gpt-5. The
    // default model allowlist is empty (all models allowed), so
    // Anthropic is a valid candidate.
    assert_eq!(
        status,
        StatusCode::OK,
        "failover to Anthropic should succeed"
    );
    assert!(
        !openai.recorded().is_empty(),
        "OpenAI should have been attempted first"
    );
    assert!(
        !anthropic.recorded().is_empty(),
        "Anthropic should have been tried as failover"
    );
}

#[tokio::test]
async fn failover_4xx_does_not_retry() {
    // A 400 (bad request) from the primary must be returned to the
    // client immediately — retrying it on another provider wastes
    // tokens and masks the real problem.
    let (state, openai, anthropic) = build_failover_state();
    openai.set_error_status(Some(400));
    let app = autorouter_server::build_router(state);
    let body = json!({
        "model": "gpt-5",
        "messages": [{ "role": "user", "content": "hi" }],
        "stream": false,
    });
    let (_status, _resp) = read_body(
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/openai/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    // 400 in the UpstreamResponse, which call_upstream_with_failover
    // sees as non-retryable).
    assert_eq!(
        openai.recorded().len(),
        1,
        "OpenAI should be called exactly once"
    );
    assert_eq!(
        anthropic.recorded().len(),
        0,
        "Anthropic must NOT be retried for a 4xx error"
    );
}

#[tokio::test]
async fn failover_4xx_from_real_upstream_path_does_not_retry() {
    // Regression: the real `HttpUpstream` returns `Err(TranslateError)`
    // for every non-2xx (not `Ok` with a status, like the mock's default
    // mode). The failover layer must still classify a 4xx as
    // non-retryable — otherwise an auth/bad-request error would silently
    // retry on other providers, wasting calls and masking the real issue.
    let (state, openai, anthropic) = build_failover_state();
    openai.set_error_status(Some(400));
    openai.set_error_as_translate_err(true);
    let app = autorouter_server::build_router(state);
    let body = json!({
        "model": "gpt-5",
        "messages": [{ "role": "user", "content": "hi" }],
        "stream": false,
    });
    let (status, _resp) = read_body(
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/openai/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    // The 4xx surfaces to the client as a 502 (the gateway's standard
    // upstream-error status) — it is NOT retried on another provider.
    assert_eq!(
        status,
        StatusCode::BAD_GATEWAY,
        "4xx upstream error should reach the client, not be swallowed"
    );
    assert_eq!(
        openai.recorded().len(),
        1,
        "OpenAI should be called exactly once"
    );
    assert_eq!(
        anthropic.recorded().len(),
        0,
        "4xx via the real Err path must NOT trigger cross-provider failover"
    );
}

#[tokio::test]
async fn failover_target_override_bypasses_retry() {
    // When the operator pins a provider via X-AutoRouter-Target,
    // the gateway respects that choice even on failure — no
    // silent retry on another provider.
    let (state, openai, anthropic) = build_failover_state();
    openai.set_error_status(Some(503));
    let app = autorouter_server::build_router(state);
    let body = json!({
        "model": "gpt-5",
        "messages": [{ "role": "user", "content": "hi" }],
        "stream": false,
    });
    let (_status, _resp) = read_body(
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/openai/v1/chat/completions")
                .header("content-type", "application/json")
                .header("x-autorouter-target", "openai")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(
        anthropic.recorded().len(),
        0,
        "X-AutoRouter-Target override must bypass failover"
    );
}

#[tokio::test]
async fn failover_all_providers_fail_returns_upstream_error_body() {
    // When every provider returns 500, the original error response
    // reaches the client. The gateway wraps the upstream body in its
    // own JSON (HTTP 200) — the upstream status is NOT propagated as
    // the HTTP status. Instead, the error is visible in the response
    // JSON body (empty content + the mock error payload).
    let (state, openai, anthropic) = build_failover_state();
    openai.set_error_status(Some(503));
    anthropic.set_error_status(Some(503));
    let app = autorouter_server::build_router(state);
    let body = json!({
        "model": "gpt-5",
        "messages": [{ "role": "user", "content": "hi" }],
        "stream": false,
    });
    let (_status, resp) = read_body(
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/openai/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    // Both providers were attempted: OpenAI as primary, Anthropic
    // as failover.
    assert!(
        !openai.recorded().is_empty(),
        "OpenAI should have been attempted"
    );
    assert!(
        !anthropic.recorded().is_empty(),
        "Anthropic should have been tried as failover"
    );
    // The response wraps the upstream error body. Since the mock
    // returned an error-shaped response with empty content, the
    // encoded OpenAI body should have empty content — a signal that
    // the upstream did not produce a real completion.
    let v: Value = serde_json::from_str(&resp).unwrap();
    let content = v
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("<missing>");
    assert!(
        content.is_empty(),
        "all-providers-failed response should have empty content (upstream error), got: {content}"
    );
}

#[tokio::test]
async fn test_openai_compatible_models_list_endpoint() {
    let pipeline = TranslationPipeline::new()
        .register(Arc::new(OpenAiChatAdapter::new()))
        .register(Arc::new(OpenAiResponsesAdapter::new()))
        .register(Arc::new(AnthropicAdapter::new()))
        .register(Arc::new(GeminiAdapter::new()));
    let openai = Arc::new(MockUpstream::new(ProviderKind::OpenAI));
    let mut upstreams: HashMap<ProviderKind, Arc<dyn autorouter_server::UpstreamClient>> =
        HashMap::new();
    upstreams.insert(ProviderKind::OpenAI, openai.clone());
    let mut config = autorouter_config::AppConfig::default();
    config.defaults.default_model = "gpt-5".to_string();
    config.defaults.default_provider = "openai".to_string();
    let state = AppState::new(config, pipeline, upstreams);
    let app = autorouter_server::build_router(state);

    // Test both endpoints: /openai/v1/models and /v1/models
    for endpoint in &["/openai/v1/models", "/v1/models"] {
        let (status, resp) = read_body(
            app.clone()
                .oneshot(
                    Request::builder()
                        .method("GET")
                        .uri(*endpoint)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;

        assert_eq!(
            status,
            StatusCode::OK,
            "Endpoint {} should respond 200",
            endpoint
        );
        let v: Value = serde_json::from_str(&resp).unwrap();

        // Assert OpenAI schema
        assert_eq!(v["object"], "list");
        let data = v["data"].as_array().expect("data field should be a list");
        assert!(!data.is_empty());

        // Assert sentinel models are present
        let mut has_autorouter = false;
        let mut has_autorouter_slash = false;

        for model in data {
            assert_eq!(model["object"], "model");
            assert!(model["owned_by"].is_string());

            let model_id = model["id"].as_str().unwrap();
            if model_id == "autorouter" {
                has_autorouter = true;
                assert_eq!(model["owned_by"], "autorouter");
            } else if model_id == "autorouter/autorouter" {
                has_autorouter_slash = true;
                assert_eq!(model["owned_by"], "autorouter");
            }
        }

        assert!(has_autorouter, "Should expose autorouter sentinel model");
        assert!(
            has_autorouter_slash,
            "Should expose autorouter/autorouter sentinel model"
        );
    }
}

#[tokio::test]
async fn standard_v1_endpoints_work() {
    let app = autorouter_server::build_router(build_state());

    // Test POST /v1/chat/completions
    let chat_body = json!({
        "model": "gpt-5",
        "messages": [{ "role": "user", "content": "hi" }],
        "stream": false,
    });
    let (status, resp_body) = read_body(
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&chat_body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "POST /v1/chat/completions should work"
    );
    let v: Value = serde_json::from_str(&resp_body).unwrap();
    assert_eq!(v["object"], "chat.completion");

    // Test POST /v1/responses
    let response_body = json!({
        "model": "gpt-5",
        "input": "hi",
        "stream": false,
    });
    let (status, resp_body) = read_body(
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&response_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "POST /v1/responses should work");
    let v: Value = serde_json::from_str(&resp_body).unwrap();
    assert_eq!(v["object"], "response");
}
