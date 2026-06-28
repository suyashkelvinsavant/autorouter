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

// --- gap #4: gateway metadata and health routes must run
// through `maybe_authorize()` when require_auth is on. Previously
// `/healthz`, `/v1/sessions`, and `/v1/models` accepted no
// `HeaderMap` and skipped auth, so any loopback-adjacent caller
// could enumerate providers / sessions / models under an
// otherwise-locked gateway.

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
