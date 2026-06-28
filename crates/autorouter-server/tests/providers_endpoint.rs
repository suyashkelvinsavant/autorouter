//! Tests for the providers endpoint dedupe and the
//! `apply_settings_patch` first-class guard.
//!
//! The Providers page in the dashboard used to render duplicate
//! cards when a custom entry in `config.toml` shared an id with
//! a first-class slot (e.g. a legacy `config.toml` that pre-dates
//! the first-class distinction, or a hand-rolled file). The fix
//! is two-layered:
//!
//!   * The GET /ui/providers handler skips any `providers.custom`
//!     entry whose key is a first-class id. The first-class card
//!     always wins so the dashboard renders exactly one card per
//!     id.
//!
//!   * The PATCH /ui/settings handler (via `apply_settings_patch`)
//!     refuses to *create* the collision in the first place. A
//!     client that posts `providers.custom.openai = ...` gets a
//!     400 with a clear error message instead of silently
//!     producing the duplicate state.
//!
//! These tests assert both halves.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use autorouter_core::ProviderKind;
use autorouter_server::ui::{
    apply_settings_patch, ProviderPatch, ProvidersPatch, SettingsPatch, UiAppState, UiState,
};
use autorouter_server::{AppState, MockUpstream, TranslationPipeline};
use autorouter_translate::{
    AnthropicAdapter, GeminiAdapter, OpenAiChatAdapter, OpenAiResponsesAdapter,
};

/// Build a router that has the UI endpoints mounted, with the
/// given `AppConfig` already installed in the UI state. Mirrors
/// the `build()` helper in `ui_api.rs` but exposes the config
/// so each test can install its own starting state.
fn build_with_cfg(cfg: autorouter_config::AppConfig) -> (Router, UiState) {
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
    let app_state = AppState::new(cfg.clone(), pipeline, upstreams);
    let ui = UiState {
        config: Arc::new(parking_lot::RwLock::new(cfg)),
        start_time: Arc::new(parking_lot::RwLock::new(chrono::Utc::now())),
        log_lines: Arc::new(parking_lot::RwLock::new(Vec::new())),
        config_path: Arc::new(parking_lot::RwLock::new(None)),
        storage: Arc::new(parking_lot::RwLock::new(None)),
        secret_store: Arc::new(parking_lot::RwLock::new(None)),
    };
    let router = autorouter_server::build_router(app_state.clone());
    let router = autorouter_server::ui::merge(
        router,
        UiAppState {
            ui: ui.clone(),
            app: app_state,
            supervisor: None,
        },
    );
    (router, ui)
}

async fn body_to_value(response: axum::response::Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

// ---------------------------------------------------------------------
// Dedupe: GET /ui/providers
// ---------------------------------------------------------------------

#[tokio::test]
async fn providers_endpoint_dedupes_custom_entry_colliding_with_first_class() {
    // Simulate a legacy `config.toml` that has BOTH a first-class
    // openai entry AND a custom entry keyed "openai". The
    // endpoint must return exactly ONE card with id="openai" so
    // the dashboard doesn't render two duplicate cards.
    let mut cfg = autorouter_config::AppConfig::default();
    cfg.providers.openai = Some(autorouter_config::ProviderEntry {
        display_name: "OpenAI".to_string(),
        base_url: "https://api.openai.com".to_string(),
        api_key_secret_id: Some("env:OPENAI_API_KEY".to_string()),
        enabled: true,
        model_allowlist: vec!["gpt-4o-mini".to_string()],
        ..Default::default()
    });
    cfg.providers.custom.insert(
        "openai".to_string(),
        autorouter_config::ProviderEntry {
            display_name: "OpenAI (custom collider)".to_string(),
            base_url: "https://api.openai.com".to_string(),
            api_key_secret_id: None,
            enabled: true,
            ..Default::default()
        },
    );
    let (router, _ui) = build_with_cfg(cfg);
    let (status, body) = body_to_value(
        router
            .oneshot(
                Request::builder()
                    .uri("/ui/providers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let providers = body
        .get("providers")
        .and_then(|p| p.as_array())
        .expect("providers array");
    let openai_cards: Vec<&Value> = providers
        .iter()
        .filter(|p| p.get("id").and_then(|id| id.as_str()) == Some("openai"))
        .collect();
    assert_eq!(
        openai_cards.len(),
        1,
        "expected exactly one openai card after dedupe, got {}",
        openai_cards.len()
    );
    // First-class card always wins; assert the deduped card is
    // the first-class one (has api_key_secret_id set, no
    // kind=custom marker).
    let card = openai_cards[0];
    assert_eq!(
        card.get("api_key_secret_id").cloned(),
        Some(json!("env:OPENAI_API_KEY"))
    );
    assert!(card.get("kind").is_none() || card.get("kind") == Some(&Value::Null));
}

#[tokio::test]
async fn providers_endpoint_preserves_non_colliding_custom_entries() {
    // Sanity: a custom entry whose id is NOT first-class must
    // still appear in the response. The dedupe loop should only
    // filter collisions, not blank every custom entry.
    let mut cfg = autorouter_config::AppConfig::default();
    cfg.providers.custom.insert(
        "groq".to_string(),
        autorouter_config::ProviderEntry {
            display_name: "Groq".to_string(),
            base_url: "https://api.groq.com".to_string(),
            enabled: true,
            model_allowlist: vec!["llama-3.1-70b".to_string()],
            ..Default::default()
        },
    );
    let (router, _ui) = build_with_cfg(cfg);
    let (status, body) = body_to_value(
        router
            .oneshot(
                Request::builder()
                    .uri("/ui/providers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let providers = body
        .get("providers")
        .and_then(|p| p.as_array())
        .expect("providers array");
    let groq_cards: Vec<&Value> = providers
        .iter()
        .filter(|p| p.get("id").and_then(|id| id.as_str()) == Some("groq"))
        .collect();
    assert_eq!(groq_cards.len(), 1);
    let card = groq_cards[0];
    assert_eq!(card.get("kind").cloned(), Some(json!("custom")));
}

// ---------------------------------------------------------------------
// Reject: PATCH /ui/settings with first-class id in `custom`
// ---------------------------------------------------------------------

#[tokio::test]
async fn patch_settings_rejects_first_class_id_in_custom() {
    // Hand-rolled PATCH that tries to write
    // `providers.custom.openai = ...`. The server must return
    // 400 with a clear error message; the in-memory config must
    // NOT be mutated.
    let cfg = autorouter_config::AppConfig::default();
    let (router, ui) = build_with_cfg(cfg);
    let patch = json!({
        "providers": {
            "custom": {
                "openai": {
                    "base_url": "https://attacker.example.com",
                    "enabled": true
                }
            }
        },
        "persist": false
    });
    let (status, body) = body_to_value(
        router
            .oneshot(
                Request::builder()
                    .method(Method::PATCH)
                    .uri("/ui/settings")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&patch).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "expected 400, body={body}");
    let msg = body
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
        .unwrap_or_default();
    assert!(
        msg.contains("openai") && msg.contains("first-class"),
        "expected error to mention first-class + id, got: {msg}"
    );
    // The in-memory config must NOT have a custom "openai" entry
    // after the rejected PATCH.
    let after = ui.config.read().clone();
    assert!(
        !after.providers.custom.contains_key("openai"),
        "rejected PATCH must not mutate providers.custom"
    );
    assert!(
        after.providers.openai.is_none(),
        "rejected PATCH must not create a first-class openai entry either"
    );
}

#[test]
fn apply_settings_patch_rejects_first_class_id_in_custom_directly() {
    // Same as the HTTP test, but exercises the pure
    // `apply_settings_patch` function so any future caller (e.g.
    // the desktop Tauri shell) gets the same protection.
    let mut cfg = autorouter_config::AppConfig::default();
    let mut custom: BTreeMap<String, ProviderPatch> = BTreeMap::new();
    custom.insert(
        "anthropic".to_string(),
        ProviderPatch {
            base_url: Some("https://attacker.example.com".to_string()),
            enabled: Some(true),
            ..Default::default()
        },
    );
    let patch = SettingsPatch {
        providers: ProvidersPatch {
            custom,
            ..Default::default()
        },
        ..Default::default()
    };
    let result = apply_settings_patch(&mut cfg, patch, None);
    assert!(result.is_err(), "expected Err for first-class collision");
    let msg = result.unwrap_err();
    assert!(msg.contains("anthropic") && msg.contains("first-class"));
    // The mutation must be rolled back: no first-class anthropic
    // entry, no custom anthropic entry.
    assert!(cfg.providers.anthropic.is_none());
    assert!(!cfg.providers.custom.contains_key("anthropic"));
}

#[test]
fn apply_settings_patch_accepts_non_colliding_custom_id() {
    // Sanity: a custom id that does NOT collide with a
    // first-class slot must still go through. Otherwise the
    // rejection would be too aggressive.
    let mut cfg = autorouter_config::AppConfig::default();
    let mut custom: BTreeMap<String, ProviderPatch> = BTreeMap::new();
    custom.insert(
        "groq".to_string(),
        ProviderPatch {
            display_name: Some("Groq".to_string()),
            base_url: Some("https://api.groq.com".to_string()),
            enabled: Some(true),
            ..Default::default()
        },
    );
    let patch = SettingsPatch {
        providers: ProvidersPatch {
            custom,
            ..Default::default()
        },
        ..Default::default()
    };
    apply_settings_patch(&mut cfg, patch, None).unwrap();
    let entry = cfg.providers.custom.get("groq").expect("groq entry");
    assert_eq!(entry.base_url, "https://api.groq.com");
    assert!(entry.enabled);
}
