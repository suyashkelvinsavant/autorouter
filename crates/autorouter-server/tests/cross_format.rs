//! Cross-format end-to-end tests.
//!
//! These tests verify the headline promise of AutoRouter: a request
//! in one provider's wire format can be served by an upstream in a
//! different provider's wire format. The test uses a recording mock
//! upstream that returns a hard-coded Anthropic-shaped body and
//! asserts the gateway reshapes it into the requested format.

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
    // Configure the Anthropic mock to return a hard-coded
    // Anthropic-shaped body. The gateway's adapter will decode the
    // body into the universal schema, then re-encode it to whatever
    // wire format the caller requested.
    let anthropic_mock = Arc::new(MockUpstream::new(ProviderKind::Anthropic));
    anthropic_mock.set_response(json!({
        "id": "msg_anthro_1",
        "type": "message",
        "role": "assistant",
        "model": "claude-sonnet-4-5",
        "content": [
            { "type": "text", "text": "hi from anthropic" }
        ],
        "stop_reason": "end_turn",
        "usage": { "input_tokens": 12, "output_tokens": 7 }
    }));
    upstreams.insert(
        ProviderKind::OpenAI,
        Arc::new(MockUpstream::new(ProviderKind::OpenAI)),
    );
    upstreams.insert(
        ProviderKind::Anthropic,
        anthropic_mock.clone() as Arc<dyn autorouter_server::UpstreamClient>,
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
async fn openai_client_targets_anthropic_upstream() {
    // Client speaks OpenAI, asks for the upstream to be Anthropic.
    let state = build_state();
    // The Anthropic mock is configured above; the gateway should re-shape the
    let app = autorouter_server::build_router(state);
    let body = json!({
        "model": "gpt-5",
        "messages": [{ "role": "user", "content": "hi" }],
    });
    let (status, body) = read_body(
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/openai/v1/chat/completions")
                .header("content-type", "application/json")
                .header("x-autorouter-target", "anthropic")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let value: Value = serde_json::from_str(&body).unwrap();
    // The response should be OpenAI Chat Completions shaped because
    // the caller impersonated OpenAI.
    assert_eq!(value["object"], "chat.completion");
    // The message content should be the Anthropic text rendered
    // into the OpenAI format.
    let text = value["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or_default();
    assert!(text.contains("hi from anthropic"), "got: {text}");
}

#[tokio::test]
async fn anthropic_client_targets_openai_upstream() {
    // Configure an OpenAI mock that returns an OpenAI-shaped body.
    let pipeline = TranslationPipeline::new()
        .register(Arc::new(OpenAiChatAdapter::new()))
        .register(Arc::new(OpenAiResponsesAdapter::new()))
        .register(Arc::new(AnthropicAdapter::new()))
        .register(Arc::new(GeminiAdapter::new()));
    let mut upstreams: HashMap<ProviderKind, Arc<dyn autorouter_server::UpstreamClient>> =
        HashMap::new();
    let openai_mock = Arc::new(MockUpstream::new(ProviderKind::OpenAI));
    openai_mock.set_response(json!({
        "id": "chatcmpl-1",
        "object": "chat.completion",
        "model": "gpt-5",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "hello openai" },
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 5, "completion_tokens": 4, "total_tokens": 9 }
    }));
    upstreams.insert(
        ProviderKind::OpenAI,
        openai_mock.clone() as Arc<dyn autorouter_server::UpstreamClient>,
    );
    upstreams.insert(
        ProviderKind::Anthropic,
        Arc::new(MockUpstream::new(ProviderKind::Anthropic)),
    );
    upstreams.insert(
        ProviderKind::Gemini,
        Arc::new(MockUpstream::new(ProviderKind::Gemini)),
    );
    let state = AppState::new(autorouter_config::AppConfig::default(), pipeline, upstreams);
    let app = autorouter_server::build_router(state);

    let body = json!({
        "model": "claude-sonnet-4-5",
        "max_tokens": 256,
        "messages": [{ "role": "user", "content": "hi" }],
    });
    let (status, body) = read_body(
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .header("x-autorouter-source", "anthropic")
                .header("x-autorouter-target", "openai")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let value: Value = serde_json::from_str(&body).unwrap();
    // The caller is Anthropic; the response should be Anthropic-shaped.
    assert_eq!(value["type"], "message");
    let content = value["content"].as_array().unwrap();
    assert!(!content.is_empty(), "expected content blocks, got: {body}");
    let text = content[0]["text"].as_str().unwrap_or_default();
    assert!(text.contains("hello openai"), "got: {text}");
    assert_eq!(value["stop_reason"], "end_turn");
}
