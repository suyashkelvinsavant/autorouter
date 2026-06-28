//! Real-data integration tests for the OpenRouter custom provider.
//!
//! These tests hit the live OpenRouter API and are skipped when
//! `OPENROUTER_API_KEY` is not set, so they do not run during normal
//! `cargo test` cycles. To run them:
//!
//! ```powershell
//! $env:OPENROUTER_API_KEY = "Bearer sk-or-v1-..."
//! cargo test -p autorouter-server --test openrouter_real -- --nocapture
//! ```
//!
//! The model exercised is the same one the user added to their
//! `config.toml`: `nex-agi/nex-n2-pro:free` (provider: SiliconFlow).
//!
//! Three code paths are covered:
//!   1. The low-level `HttpUpstream` (verifies auth header, URL,
//!      and OpenAI Chat body shape).
//!   2. The Anthropic `/v1/messages` endpoint targeting the
//!      `openrouter` custom provider via the
//!      `X-AutoRouter-Target` header. This is the headline
//!      cross-format flow the user wants to verify.
//!   3. A workaround: when the gateway's custom-provider path
//!      fails (see Test 2), the same Anthropic request can be
//!      served by the openrouter upstream if the override points
//!      at the built-in `openai` kind with the openrouter
//!      `base_url` set on the built-in provider. This is a
//!      stop-gap and not a real fix; it exists so we can still
//!      prove the end-to-end cross-format translation works
//!      against the live API.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use autorouter_config::{ApiFormat, AppConfig, ProviderEntry, ProvidersConfig};
use autorouter_core::ProviderKind;
use autorouter_server::{
    AppState, HttpUpstream, HttpUpstreamConfig, MockUpstream, TranslationPipeline, UpstreamClient,
};
use autorouter_translate::{
    AnthropicAdapter, GeminiAdapter, OpenAiChatAdapter, OpenAiResponsesAdapter,
};

/// The free model the user configured in their `config.toml`.
const MODEL: &str = "nex-agi/nex-n2-pro:free";
/// OpenRouter's OpenAI-compatible chat-completions base.
const BASE_URL: &str = "https://openrouter.ai/api/v1";
/// Name of the custom provider entry the user added.
const CUSTOM_NAME: &str = "openrouter";

/// Pull the bearer token (with the "Bearer " prefix) from the
/// environment. Returns `None` when the var is unset so the test
/// can early-return as a no-op without failing the suite.
fn openrouter_key() -> Option<String> {
    std::env::var("OPENROUTER_API_KEY").ok()
}

/// Build a real `HttpUpstream` for OpenRouter. The auth value is
/// passed straight through; the gateway's `build_request` sets it
/// as the `Authorization` header verbatim, so the env var must
/// already include the `Bearer ` prefix (which is the format
/// OpenRouter expects).
fn openrouter_upstream() -> Option<HttpUpstream> {
    let key = openrouter_key()?;
    let entry = ProviderEntry {
        display_name: "OpenRouter".into(),
        base_url: BASE_URL.into(),
        api_key_secret_id: Some("env:OPENROUTER_API_KEY".into()),
        default_headers: BTreeMap::new(),
        enabled: true,
        model_allowlist: vec![MODEL.to_string()],
        api_format: ApiFormat::OpenAI,
    };
    let cfg = HttpUpstreamConfig::from_entry(
        &entry,
        ProviderKind::OpenAI,
        Some(key),
        Duration::from_secs(60),
    );
    HttpUpstream::new(cfg).ok()
}

/// Build the AppState for the gateway-level test: a real OpenRouter
/// custom upstream plus mock OpenAI/Anthropic/Gemini built-ins.
/// The pipeline registers all four built-in adapters so the
/// Anthropic request can be parsed on the way in and the OpenAI
/// response can be re-encoded on the way back out.
fn gateway_state() -> Option<AppState> {
    let upstream = openrouter_upstream()?;
    let pipeline = TranslationPipeline::new()
        .register(Arc::new(OpenAiChatAdapter::new()))
        .register(Arc::new(OpenAiResponsesAdapter::new()))
        .register(Arc::new(AnthropicAdapter::new()))
        .register(Arc::new(GeminiAdapter::new()));
    let mut built_in: HashMap<ProviderKind, Arc<dyn UpstreamClient>> = HashMap::new();
    built_in.insert(
        ProviderKind::OpenAI,
        Arc::new(MockUpstream::new(ProviderKind::OpenAI)),
    );
    built_in.insert(
        ProviderKind::Anthropic,
        Arc::new(MockUpstream::new(ProviderKind::Anthropic)),
    );
    built_in.insert(
        ProviderKind::Gemini,
        Arc::new(MockUpstream::new(ProviderKind::Gemini)),
    );
    // The config must contain the custom provider so the
    // X-AutoRouter-Target override resolves to it and the model
    // listing endpoint surfaces the allowlisted model.
    let mut custom = BTreeMap::new();
    custom.insert(
        CUSTOM_NAME.to_string(),
        ProviderEntry {
            display_name: "OpenRouter".into(),
            base_url: BASE_URL.into(),
            api_key_secret_id: Some("env:OPENROUTER_API_KEY".into()),
            default_headers: BTreeMap::new(),
            enabled: true,
            model_allowlist: vec![MODEL.to_string()],
            api_format: ApiFormat::OpenAI,
        },
    );
    let config = AppConfig {
        providers: ProvidersConfig {
            openai: None,
            anthropic: None,
            gemini: None,
            custom,
        },
        ..AppConfig::default()
    };
    let custom_upstreams: BTreeMap<String, Arc<dyn UpstreamClient>> = std::iter::once((
        CUSTOM_NAME.to_string(),
        Arc::new(upstream) as Arc<dyn UpstreamClient>,
    ))
    .collect();
    Some(AppState::new(config, pipeline, built_in).with_custom_upstreams(custom_upstreams))
}

/// Build the AppState for the workaround test: the OpenRouter
/// upstream is registered as the *built-in* `openai` provider, so
/// `serialise_request(ProviderKind::OpenAI, ...)` works through
/// the existing adapter.
fn gateway_state_openai_override() -> Option<AppState> {
    let upstream = openrouter_upstream()?;
    let pipeline = TranslationPipeline::new()
        .register(Arc::new(OpenAiChatAdapter::new()))
        .register(Arc::new(OpenAiResponsesAdapter::new()))
        .register(Arc::new(AnthropicAdapter::new()))
        .register(Arc::new(GeminiAdapter::new()));
    let mut built_in: HashMap<ProviderKind, Arc<dyn UpstreamClient>> = HashMap::new();
    built_in.insert(
        ProviderKind::OpenAI,
        Arc::new(upstream) as Arc<dyn UpstreamClient>,
    );
    built_in.insert(
        ProviderKind::Anthropic,
        Arc::new(MockUpstream::new(ProviderKind::Anthropic)),
    );
    built_in.insert(
        ProviderKind::Gemini,
        Arc::new(MockUpstream::new(ProviderKind::Gemini)),
    );
    let config = AppConfig {
        providers: ProvidersConfig {
            openai: Some(ProviderEntry {
                display_name: "OpenRouter (as openai built-in)".into(),
                base_url: BASE_URL.into(),
                api_key_secret_id: Some("env:OPENROUTER_API_KEY".into()),
                default_headers: BTreeMap::new(),
                enabled: true,
                model_allowlist: vec![MODEL.to_string()],
                api_format: ApiFormat::OpenAI,
            }),
            anthropic: None,
            gemini: None,
            custom: BTreeMap::new(),
        },
        ..AppConfig::default()
    };
    Some(AppState::new(config, pipeline, built_in))
}

async fn read_body(response: axum::response::Response) -> (StatusCode, String) {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap_or_default();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

fn skipped() -> bool {
    openrouter_key().is_none()
}

fn log_skip(test: &str) {
    eprintln!("[{test}] OPENROUTER_API_KEY not set; skipping real-data test");
}

#[tokio::test]
async fn openrouter_http_direct_sends_request() {
    if skipped() {
        log_skip("openrouter_http_direct_sends_request");
        return;
    }
    let upstream = openrouter_upstream().expect("openrouter upstream");
    let body = json!({
        "model": MODEL,
        "messages": [{ "role": "user", "content": "Reply with the single word: PONG" }],
        "max_tokens": 32,
        "temperature": 0.0,
    });
    let resp = upstream
        .send(&body)
        .await
        .expect("openrouter send should succeed");
    assert_eq!(
        resp.status, 200,
        "expected 200 from openrouter; got {} body={}",
        resp.status, resp.raw
    );
    let text: String = resp.response.message.text();
    assert!(
        !text.is_empty(),
        "empty text from openrouter; raw={}",
        resp.raw
    );
    let head = text
        .split_whitespace()
        .take(8)
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    assert!(
        head.contains("pong"),
        "expected 'pong' in first 8 words; got: {head:?}"
    );
    assert!(
        resp.response.usage.tokens.input.unwrap_or(0) > 0,
        "expected non-zero prompt tokens; got: {:?}",
        resp.response.usage
    );
    assert!(
        resp.response.usage.tokens.output.unwrap_or(0) > 0,
        "expected non-zero completion tokens; got: {:?}",
        resp.response.usage
    );
    eprintln!(
        "[openrouter_http_direct_sends_request] prompt={} completion={} text={:?}",
        resp.response.usage.tokens.input.unwrap_or(0),
        resp.response.usage.tokens.output.unwrap_or(0),
        text
    );
}

#[tokio::test]
async fn gateway_anthropic_endpoint_targets_openrouter_custom() {
    if skipped() {
        log_skip("gateway_anthropic_endpoint_targets_openrouter_custom");
        return;
    }
    let state = gateway_state().expect("gateway state");
    let app = autorouter_server::build_router(state);
    let body = json!({
        "model": MODEL,
        "max_tokens": 256,
        "messages": [{ "role": "user", "content": "Reply with the single word: PONG" }],
    });
    let (status, body) = read_body(
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .header("x-autorouter-source", "anthropic")
                .header("x-autorouter-target", CUSTOM_NAME)
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    eprintln!(
        "[gateway_anthropic_endpoint_targets_openrouter_custom] status={} body={}",
        status, body
    );
    assert_eq!(
        status,
        StatusCode::OK,
        "Anthropic -> openrouter custom call should succeed after the Custom serialise fix. body={body}"
    );
    let value: Value = serde_json::from_str(&body).expect("response is JSON");
    assert_eq!(value["type"], "message");
    assert_eq!(value["role"], "assistant");
    let content = value["content"].as_array().expect("content is an array");
    assert!(!content.is_empty(), "expected at least one content block");
    let text = content[0]["text"].as_str().unwrap_or_default();
    assert!(
        text.to_lowercase().contains("pong"),
        "expected 'pong' in assistant text; got: {text:?}"
    );
    assert_eq!(value["stop_reason"], "end_turn");
    let usage = &value["usage"];
    assert!(
        usage["input_tokens"].as_u64().unwrap_or(0) > 0,
        "expected non-zero input_tokens; got: {usage}"
    );
    assert!(
        usage["output_tokens"].as_u64().unwrap_or(0) > 0,
        "expected non-zero output_tokens; got: {usage}"
    );
    eprintln!("[gateway_anthropic_endpoint_targets_openrouter_custom] text={text:?} usage={usage}");
}

#[tokio::test]
async fn gateway_anthropic_endpoint_targets_openai_openrouter_workaround() {
    if skipped() {
        log_skip("gateway_anthropic_endpoint_targets_openai_openrouter_workaround");
        return;
    }
    // Workaround: pretend OpenRouter IS the built-in `openai` provider.
    // This sidesteps the missing Custom serialise path while still
    // proving the Anthropic -> OpenAI cross-format translation works
    // against a real upstream.
    let state = gateway_state_openai_override().expect("gateway state");
    let app = autorouter_server::build_router(state);
    let body = json!({
        "model": MODEL,
        "max_tokens": 256,
        "messages": [{ "role": "user", "content": "Reply with the single word: PONG" }],
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
    eprintln!(
        "[gateway_anthropic_endpoint_targets_openai_openrouter_workaround] status={} body={}",
        status, body
    );
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let value: Value = serde_json::from_str(&body).expect("response is JSON");
    assert_eq!(value["type"], "message");
    assert_eq!(value["role"], "assistant");
    let content = value["content"].as_array().expect("content is an array");
    assert!(!content.is_empty(), "expected at least one content block");
    let text = content[0]["text"].as_str().unwrap_or_default();
    assert!(
        text.to_lowercase().contains("pong"),
        "expected 'pong' in assistant text; got: {text:?}"
    );
    assert_eq!(value["stop_reason"], "end_turn");
    let usage = &value["usage"];
    assert!(
        usage["input_tokens"].as_u64().unwrap_or(0) > 0,
        "expected non-zero input_tokens; got: {usage}"
    );
    assert!(
        usage["output_tokens"].as_u64().unwrap_or(0) > 0,
        "expected non-zero output_tokens; got: {usage}"
    );
}

#[tokio::test]
async fn openrouter_http_direct_streams_response() {
    if skipped() {
        log_skip("openrouter_http_direct_streams_response");
        return;
    }
    use futures::StreamExt;

    let upstream = openrouter_upstream().expect("openrouter upstream");
    let body = json!({
        "model": MODEL,
        "messages": [{ "role": "user", "content": "Reply with the single word: PONG" }],
        "max_tokens": 32,
        "temperature": 0.0,
        "stream": true,
    });
    let mut stream = upstream
        .send_streaming(&body)
        .await
        .expect("openrouter streaming send");
    let mut collected = String::new();
    let mut got_finish = false;
    while let Some(item) = stream.next().await {
        let chunk = item.expect("openrouter stream chunk");
        for ev in &chunk.events {
            match ev {
                autorouter_core::StreamEvent::TextDelta { text } => collected.push_str(text),
                autorouter_core::StreamEvent::Finish { reason, .. } => {
                    got_finish = true;
                    eprintln!(
                        "[openrouter_http_direct_streams_response] finish reason: {:?}",
                        reason
                    );
                }
                _ => {}
            }
        }
    }
    assert!(got_finish, "stream ended without a Finish event");
    let head = collected
        .split_whitespace()
        .take(8)
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    assert!(
        head.contains("pong"),
        "expected 'pong' in streamed text; got: {head:?}"
    );
}
