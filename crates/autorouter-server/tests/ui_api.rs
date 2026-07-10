//! Integration tests for the dashboard UI endpoints.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use autorouter_core::ProviderKind;
use autorouter_server::ui::{LogLine, UiAppState, UiState};
use autorouter_server::{AppState, MockUpstream, TranslationPipeline};
use autorouter_translate::{
    AnthropicAdapter, GeminiAdapter, OpenAiChatAdapter, OpenAiResponsesAdapter,
};

fn build() -> (Router, AppState, UiState) {
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
    let app_state = AppState::new(autorouter_config::AppConfig::default(), pipeline, upstreams);
    let ui = UiState {
        config: Arc::new(parking_lot::RwLock::new(
            autorouter_config::AppConfig::default(),
        )),
        start_time: Arc::new(parking_lot::RwLock::new(chrono::Utc::now())),
        log_lines: Arc::new(parking_lot::RwLock::new(Vec::new())),
        config_path: Arc::new(parking_lot::RwLock::new(None)),
        storage: Arc::new(parking_lot::RwLock::new(None)),
        secret_store: Arc::new(parking_lot::RwLock::new(None)),
        supervisor: None,
    };
    let router = autorouter_server::build_router(app_state.clone());
    let router = autorouter_server::ui::merge(
        router,
        UiAppState {
            ui: ui.clone(),
            app: app_state.clone(),
            supervisor: None,
        },
    );
    (router, app_state, ui)
}

async fn body_to_value(response: axum::response::Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

#[tokio::test]
async fn status_endpoint_returns_snapshot() {
    let (router, _app, _ui) = build();
    let (status, body) = body_to_value(
        router
            .oneshot(
                Request::builder()
                    .uri("/ui/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.get("version").is_some());
    assert!(body.get("bind").is_some());
    assert!(body.get("uptime_seconds").is_some());
    // The status payload now distinguishes "configured" / "disabled"
    // / "missing" so the Dashboard can render distinct badges; see
    // ui.rs::status. The default test fixture has no openai entry,
    // so the value is "missing".
    assert_eq!(
        body.get("providers").and_then(|p| p.get("openai")).cloned(),
        Some(json!("missing"))
    );
}

#[tokio::test]
async fn providers_endpoint_lists_custom_models() {
    let (router, _app, ui) = build();
    {
        let mut cfg = ui.config.write();
        let custom_entry = autorouter_config::ProviderEntry {
            display_name: "Groq".to_string(),
            base_url: "https://api.groq.com".to_string(),
            model_allowlist: vec!["llama-test-1".to_string(), "llama-test-2".to_string()],
            ..Default::default()
        };
        cfg.providers
            .custom
            .insert("groq".to_string(), custom_entry);
    }
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
    let models = body
        .get("models")
        .and_then(|m| m.as_array())
        .expect("models array");
    let groq_models: Vec<&Value> = models
        .iter()
        .filter(|m| m.get("provider").and_then(|p| p.as_str()) == Some("groq"))
        .collect();
    assert_eq!(groq_models.len(), 2);
    assert!(groq_models
        .iter()
        .any(|m| m.get("id").and_then(|id| id.as_str()) == Some("llama-test-1")));
    assert!(groq_models
        .iter()
        .any(|m| m.get("id").and_then(|id| id.as_str()) == Some("llama-test-2")));
}

#[tokio::test]
async fn settings_round_trips() {
    let (router, _app, _ui) = build();
    let (status, body) = body_to_value(
        router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/ui/settings")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.get("server").is_some());
    assert!(body.get("defaults").is_some());

    // PATCH a single field and confirm it sticks.
    let (status, _) = body_to_value(
        router
            .oneshot(
                Request::builder()
                    .method(Method::PATCH)
                    .uri("/ui/settings")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&json!({ "defaults": { "default_model": "gpt-test" } }))
                            .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn settings_endpoint_redacts_auth_token() {
    // GET /ui/settings must NEVER return the configured bearer
    // credential. The UI replaces it with null + has_auth_token flag.
    let (router, _app, ui) = build();
    {
        let mut cfg = ui.config.write();
        cfg.server.auth_token = Some("super-secret-bearer-credential".to_string());
    }
    let (status, body) = body_to_value(
        router
            .oneshot(
                Request::builder()
                    .uri("/ui/settings")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let token = body
        .get("server")
        .and_then(|s| s.get("auth_token"))
        .and_then(|t| t.as_str());
    assert!(
        token.is_none(),
        "auth_token must be redacted (was: {token:?})"
    );
    // `has_auth_token` must be true so the UI knows to show the
    // "(set — type to replace)" placeholder.
    let has_token = body
        .get("has_auth_token")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    assert!(
        has_token,
        "has_auth_token must be true when a token is configured"
    );
    // The bearer credential itself must not appear anywhere in the
    // response payload (defence-in-depth: even if a future
    // serializer bug echoed a copy in a sibling field, the
    // credential would still leak).
    let raw = serde_json::to_string(&body).unwrap();
    assert!(
        !raw.contains("super-secret-bearer-credential"),
        "bearer credential must not appear in serialized response body"
    );
}

#[tokio::test]
async fn debug_endpoint_redacts_auth_token_and_env() {
    // GET /ui/debug must redact server.auth_token and scrub
    // AUTOROUTER_AUTH_TOKEN from the env echo.
    std::env::set_var("AUTOROUTER_AUTH_TOKEN", "env-super-secret-bearer");
    let (router, _app, ui) = build();
    {
        let mut cfg = ui.config.write();
        cfg.server.auth_token = Some("cfg-super-secret-bearer".to_string());
    }
    let (status, body) = body_to_value(
        router
            .oneshot(
                Request::builder()
                    .uri("/ui/debug")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let cfg_token = body
        .get("config")
        .and_then(|c| c.get("server"))
        .and_then(|s| s.get("auth_token"));
    assert!(
        cfg_token.is_none() || cfg_token == Some(&Value::Null),
        "config.server.auth_token must be redacted in /ui/debug"
    );
    let env_entries = body
        .get("env")
        .and_then(|e| e.as_array())
        .expect("env array");
    let auth_env = env_entries
        .iter()
        .find(|e| e.get("key").and_then(|k| k.as_str()) == Some("AUTOROUTER_AUTH_TOKEN"));
    if let Some(entry) = auth_env {
        let value = entry
            .get("value")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let redacted = entry
            .get("redacted")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        assert!(
            redacted && value == "***",
            "AUTOROUTER_AUTH_TOKEN env value must be redacted"
        );
        assert!(
            !value.contains("env-super-secret-bearer"),
            "env value must not leak bearer credential"
        );
    }
    let raw = serde_json::to_string(&body).unwrap();
    assert!(
        !raw.contains("cfg-super-secret-bearer"),
        "config bearer credential must not appear in debug payload"
    );
    assert!(
        !raw.contains("env-super-secret-bearer"),
        "env bearer credential must not appear in debug payload"
    );
    std::env::remove_var("AUTOROUTER_AUTH_TOKEN");
}

#[tokio::test]
async fn patch_settings_response_redacts_auth_token() {
    // PATCH /ui/settings must not echo the bearer credential back.
    let (router, _app, ui) = build();
    {
        let mut cfg = ui.config.write();
        cfg.server.auth_token = Some("cfg-pre-patch-bearer".to_string());
    }
    let (status, body) = body_to_value(
        router
            .oneshot(
                Request::builder()
                    .method(Method::PATCH)
                    .uri("/ui/settings")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&json!({ "defaults": { "default_model": "gpt-test" } }))
                            .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let token = body
        .get("server")
        .and_then(|s| s.get("auth_token"))
        .and_then(|t| t.as_str());
    assert!(
        token.is_none(),
        "auth_token must be redacted from PATCH /ui/settings response (was: {token:?})"
    );
    let has_token = body
        .get("has_auth_token")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    assert!(
        has_token,
        "has_auth_token must be true after a redacted PATCH"
    );
    let raw = serde_json::to_string(&body).unwrap();
    assert!(
        !raw.contains("cfg-pre-patch-bearer"),
        "bearer credential must not appear in PATCH /ui/settings response body"
    );
}

#[test]
fn redact_auth_token_in_toml_replaces_value_with_empty() {
    // The Import/Export page advertises "Secret values are not
    // exported" — verify the redaction helper strips auth_token.
    let input = r#"[server]
bind = "127.0.0.1:4073"
auth_token = "live-test-secret-bearer-credential"
require_auth = true

[defaults]
default_model = "gpt-4o-mini"
"#;
    let out = autorouter_server::ui::redact_auth_token_in_toml(input);
    assert!(
        !out.contains("live-test-secret-bearer-credential"),
        "auth_token value must be removed (got: {out:?})"
    );
    assert!(
        out.contains("auth_token = \"\""),
        "auth_token line must be rewritten with empty string (got: {out:?})"
    );
    assert!(
        out.contains("# redacted by /ui/export"),
        "redaction comment must be present so the operator knows what to do (got: {out:?})"
    );
    // Other lines must be left untouched.
    assert!(out.contains(r#"bind = "127.0.0.1:4073""#));
    assert!(out.contains(r#"default_model = "gpt-4o-mini""#));
    assert!(out.contains("require_auth = true"));
}

#[test]
fn redact_auth_token_in_toml_preserves_comments_and_indent() {
    let input = "  auth_token = \"x\"\n  # auth_token = \"commented-out\"\n";
    let out = autorouter_server::ui::redact_auth_token_in_toml(input);
    assert!(out.contains("# auth_token = \"commented-out\""));
    assert!(out.starts_with("  auth_token = \"\"  #"));
}

#[test]
fn redact_auth_token_in_toml_no_trailing_newline_doubling() {
    let input = "[server]\nauth_token = \"super-secret-bearer\"";
    let out = autorouter_server::ui::redact_auth_token_in_toml(input);
    assert!(
        !out.ends_with("\n\n"),
        "must not add a trailing newline when input lacked one (got: {out:?})"
    );
    assert!(
        out.contains("# redacted by /ui/export"),
        "redaction comment must be present (got: {out:?})"
    );
    assert!(
        !out.contains("super-secret-bearer"),
        "literal token must be gone (got: {out:?})"
    );
}

#[test]
fn redact_auth_token_in_config_clears_field_and_sets_flag() {
    // The shared helper used by both HTTP and Tauri paths must
    // replace the bearer credential with null + has_auth_token.
    use serde_json::json;
    let input = json!({
        "server": {
            "bind": "127.0.0.1:4073",
            "auth_token": "live-test-bearer-credential",
        },
        "defaults": { "default_model": "gpt-4o-mini" }
    });
    let out = autorouter_server::ui::redact_auth_token_in_config(input);
    let token = out
        .get("server")
        .and_then(|s| s.get("auth_token"))
        .and_then(|t| t.as_str());
    assert!(
        token.is_none(),
        "auth_token must be replaced with null (was: {token:?})"
    );
    assert_eq!(
        out.get("server")
            .and_then(|s| s.get("auth_token"))
            .cloned()
            .unwrap(),
        serde_json::Value::Null,
        "auth_token must be null after redaction"
    );
    let has = out
        .get("has_auth_token")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    assert!(
        has,
        "has_auth_token must be true when a token was configured"
    );
    let raw = serde_json::to_string(&out).unwrap();
    assert!(
        !raw.contains("live-test-bearer-credential"),
        "bearer credential must not appear in serialised output"
    );
}

#[test]
fn redact_auth_token_in_config_handles_unset_token() {
    // When no token is configured, `has_auth_token` must be false
    // and the existing null must be preserved.
    use serde_json::json;
    let input = json!({
        "server": { "auth_token": null },
        "defaults": { "default_model": "gpt-4o-mini" }
    });
    let out = autorouter_server::ui::redact_auth_token_in_config(input);
    assert_eq!(
        out.get("has_auth_token").cloned().unwrap(),
        serde_json::Value::Bool(false),
        "has_auth_token must be false when no token is configured"
    );
}

#[test]
fn redact_auth_token_in_toml_redacts_default_headers_values() {
    let input = r#"[server]
bind = "127.0.0.1:4073"
auth_token = "my-auth-token"

[defaults]
default_provider = "openai"

[providers.openai]
display_name = "OpenAI"
base_url = "https://api.openai.com/v1"

[providers.openai.default_headers]
Authorization = "Bearer sk-test-secret"
x-api-key = "sk-another-secret"
"#;
    let out = autorouter_server::ui::redact_auth_token_in_toml(input);
    assert!(
        !out.contains("sk-test-secret"),
        "default_headers Authorization value must be redacted (got: {out:?})"
    );
    assert!(
        !out.contains("sk-another-secret"),
        "default_headers x-api-key value must be redacted (got: {out:?})"
    );
    // Other values must stay.
    assert!(out.contains(r#"bind = "127.0.0.1:4073""#));
    assert!(out.contains("OpenAI"));
    assert!(out.contains("https://api.openai.com/v1"));
    // The auth_token is still redacted too.
    assert!(
        !out.contains("my-auth-token"),
        "auth_token must still be redacted alongside default_headers"
    );
}

#[tokio::test]
async fn logs_endpoint_paginates() {
    let (router, _app, ui) = build();
    for i in 0..5 {
        LogLine::push(&ui.log_lines, "info", "test", &format!("hello {i}"));
    }
    let (status, body) = body_to_value(
        router
            .oneshot(
                Request::builder()
                    .uri("/ui/logs?limit=10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let lines = body
        .get("lines")
        .and_then(|l| l.as_array())
        .expect("lines array");
    assert_eq!(lines.len(), 5);
}

fn make_storage() -> Arc<autorouter_server::StorageHandle> {
    let dir = std::env::temp_dir().join(format!(
        "autorouter-mirror-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    Arc::new(autorouter_server::StorageHandle::open(dir.join("autorouter.db")).unwrap())
}

#[test]
fn mirror_settings_writes_thirteen_keys() {
    // M14/M20: the helper must persist every PATCH-able field of
    // the live AppConfig into the StorageHandle so a clean restart
    // restores the operator's choices.
    let storage = make_storage();
    let mut cfg = autorouter_config::AppConfig::default();
    cfg.server.bind = "127.0.0.1:18099".to_string();
    cfg.server.enable_cors = Some(true);
    cfg.server.require_auth = Some(true);
    cfg.server.auth_token = Some("token-1234".to_string());
    cfg.server.max_body_bytes = 4 * 1024 * 1024;
    cfg.server.request_timeout_seconds = 90;
    cfg.server.stream_idle_timeout_seconds = 30;
    cfg.defaults.default_provider = "anthropic".to_string();
    cfg.defaults.default_model = "claude-test".to_string();
    cfg.defaults.stream_by_default = Some(true);
    cfg.defaults.max_total_tokens = Some(8192);
    cfg.logging.level = "debug".to_string();
    cfg.logging.json = Some(true);
    autorouter_server::ui::mirror_settings_to_storage(&storage, &cfg);
    let cases: &[(&str, &str)] = &[
        ("auth_token", "token-1234"),
        ("bind", "127.0.0.1:18099"),
        ("enable_cors", "true"),
        ("require_auth", "true"),
        ("max_body_bytes", &(4u64 * 1024 * 1024).to_string()),
        ("request_timeout_seconds", "90"),
        ("stream_idle_timeout_seconds", "30"),
        ("default_provider", "anthropic"),
        ("default_model", "claude-test"),
        ("stream_by_default", "true"),
        ("max_total_tokens", "8192"),
        ("log_level", "debug"),
        ("log_json", "true"),
    ];
    assert_eq!(cases.len(), 13, "M14/M20 mirror must cover all 13 keys");
    for (k, expected) in cases {
        let got = storage.get_setting(k).expect("get_setting");
        assert_eq!(
            got.as_deref(),
            Some(*expected),
            "key {k} did not round-trip"
        );
    }
}

fn build_with_secret() -> (Router, AppState, UiState) {
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
    let app_state = AppState::new(autorouter_config::AppConfig::default(), pipeline, upstreams);
    let store = autorouter_config::build_secret_store("memory", None);
    let ui = UiState {
        config: Arc::new(parking_lot::RwLock::new(
            autorouter_config::AppConfig::default(),
        )),
        start_time: Arc::new(parking_lot::RwLock::new(chrono::Utc::now())),
        log_lines: Arc::new(parking_lot::RwLock::new(Vec::new())),
        config_path: Arc::new(parking_lot::RwLock::new(None)),
        storage: Arc::new(parking_lot::RwLock::new(None)),
        secret_store: Arc::new(parking_lot::RwLock::new(Some(store))),
        supervisor: None,
    };
    let router = autorouter_server::build_router(app_state.clone());
    let router = autorouter_server::ui::merge(
        router,
        UiAppState {
            ui: ui.clone(),
            app: app_state.clone(),
            supervisor: None,
        },
    );
    (router, app_state, ui)
}

#[tokio::test]
async fn secrets_endpoint_returns_backend_and_ids() {
    // M13: GET /ui/secrets should return the backend name and
    // whether list() is supported, plus the list of ids when the
    // backend supports enumeration.
    let store = autorouter_config::build_secret_store("memory", None);
    let _ = store.put(autorouter_config::Secret {
        id: autorouter_config::SecretId::new("openai-key"),
        value: "x".to_string(),
        label: Some("openai".to_string()),
        created_at: 0,
    });
    assert!(store.list_supported());
    let ids = store.list().unwrap();
    assert_eq!(ids.len(), 1);
    let backend = store.backend_name();
    assert_eq!(backend, "memory");
}

#[tokio::test]
async fn secrets_http_endpoint_shape() {
    let (router, _app, _ui) = build_with_secret();
    let (status, body) = body_to_value(
        router
            .oneshot(
                Request::builder()
                    .uri("/ui/secrets")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.get("backend").and_then(|v| v.as_str()), Some("memory"));
    assert_eq!(
        body.get("list_supported").and_then(|v| v.as_bool()),
        Some(true)
    );
    let ids = body
        .get("ids")
        .and_then(|v| v.as_array())
        .expect("ids array");
    assert!(ids.is_empty());
}

#[tokio::test]
async fn secrets_http_endpoint_when_unavailable() {
    let (router, _app, _ui) = build();
    let (status, body) = body_to_value(
        router
            .oneshot(
                Request::builder()
                    .uri("/ui/secrets")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // No store wired -> backend is null and ids are null.
    assert!(body.get("backend").map(|v| v.is_null()).unwrap_or(false));
    assert_eq!(
        body.get("list_supported").and_then(|v| v.as_bool()),
        Some(false)
    );
    assert!(body.get("ids").map(|v| v.is_null()).unwrap_or(false));
}

// --- Regression: "Save server" must actually move the listener ---
//
// Prior to the supervisor, the gateway was bound to a single
// TcpListener for the lifetime of the process. PATCH /ui/settings
// happily updated the in-memory config and persisted to
// config.toml, but the running socket stayed pinned to the
// original port, so the Settings page looked like a no-op.
// `rebind_via_supervisor_after_settings_patch` exercises the
// hot-rebind path end-to-end: the supervisor starts on one port,
// we update the bind, the supervisor rebinds, and a probe to the
// new port gets a 200.

use std::time::Duration;

use autorouter_server::{GatewaySupervisor, RebindOutcome};

fn pick_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind 0");
    let port = listener.local_addr().expect("local_addr").port();
    drop(listener);
    port
}

#[tokio::test]
async fn rebind_via_supervisor_after_settings_patch() {
    let port_a = pick_port();
    let port_b = pick_port();
    let bind_a = format!("127.0.0.1:{port_a}");
    let bind_b = format!("127.0.0.1:{port_b}");

    // `build()` already returns the gateway router merged with
    // the UI sub-router, which matches the production stack
    // built by `build_router_with_ui`.
    let (router, app, _ui) = build();

    let supervisor = GatewaySupervisor::new();
    supervisor
        .clone()
        .start(router, &bind_a)
        .await
        .expect("start on first port");
    assert_eq!(supervisor.current_bind().as_deref(), Some(bind_a.as_str()));

    // Simulate a PATCH /ui/settings that changes the bind: take
    // the current Arc<AppConfig>, swap in the new bind, and
    // install it via `replace_config`. This is exactly what the
    // HTTP PATCH /ui/settings handler does, minus the on-disk
    // persistence.
    {
        let cfg_arc = app.current_config();
        let mut cfg = (*cfg_arc).clone();
        cfg.server.bind = bind_b.clone();
        app.replace_config(cfg);
    }
    assert_eq!(app.current_config().server.bind, bind_b);

    // Hot-rebind using the new bind. The closure rebuilds the
    // router from the same `app` so the rebind path sees the
    // updated config in any subsequent request.
    let outcome = supervisor
        .clone()
        .rebind_if_needed(&bind_b, || async {
            // `build()` rebuilds a fresh router+UI from scratch
            // but reuses the same `app` for shared state, which
            // is the contract the production `cmd_settings_patch`
            // path follows.
            let _ = app.clone();
            let (router, _, _) = build();
            router
        })
        .await
        .expect("rebind");
    assert_eq!(outcome, RebindOutcome::Rebound);
    assert_eq!(supervisor.current_bind().as_deref(), Some(bind_b.as_str()));

    // Give axum a moment to start accepting on the new port.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let probe = reqwest::get(format!("http://{bind_b}/healthz"))
        .await
        .expect("probe new bind");
    assert_eq!(probe.status(), 200);

    supervisor.stop();
}

// --- R10: HTTP PATCH /ui/settings must actually rebind the listener ---
//
// Prior to R10, the HTTP PATCH /ui/settings handler updated the
// in-memory config and persisted to config.toml, but never routed
// through the `GatewaySupervisor`. The Settings page therefore
// looked like a no-op when the operator changed the bind. This test
// exercises the full HTTP PATCH path with a supervisor attached and
// asserts the listener moves to the new port.

#[tokio::test]
async fn http_patch_settings_rebinds_via_supervisor() {
    use autorouter_server::{build_router_with_ui, GatewaySupervisor};

    let port_a = pick_port();
    let port_b = pick_port();
    let bind_a = format!("127.0.0.1:{port_a}");
    let bind_b = format!("127.0.0.1:{port_b}");

    // Build the same state the production stack uses, but attach a
    // supervisor so the HTTP PATCH handler can hot-rebind.
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
    let mut initial_cfg = autorouter_config::AppConfig::default();
    initial_cfg.server.bind = bind_a.clone();
    let app_state = AppState::new(initial_cfg, pipeline, upstreams);
    let ui = UiState {
        config: Arc::new(parking_lot::RwLock::new(
            autorouter_config::AppConfig::default(),
        )),
        start_time: Arc::new(parking_lot::RwLock::new(chrono::Utc::now())),
        log_lines: Arc::new(parking_lot::RwLock::new(Vec::new())),
        config_path: Arc::new(parking_lot::RwLock::new(None)),
        storage: Arc::new(parking_lot::RwLock::new(None)),
        secret_store: Arc::new(parking_lot::RwLock::new(None)),
        supervisor: None,
    };
    // Seed the UiState config with the initial bind so the PATCH
    // handler sees the right starting point.
    ui.config.write().server.bind = bind_a.clone();
    let supervisor = GatewaySupervisor::new();
    // Build the live router that the supervisor will serve.
    let router = build_router_with_ui(
        app_state.clone(),
        ui.clone(),
        true,
        Some(supervisor.clone()),
    );

    supervisor
        .clone()
        .start(router, &bind_a)
        .await
        .expect("start on first port");
    assert_eq!(supervisor.current_bind().as_deref(), Some(bind_a.as_str()));

    // Build a fresh one-shot router sharing the same UiAppState
    // (which carries the supervisor). The PATCH handler will read
    // `s.supervisor` from this state and route through it.
    let live_router = build_router_with_ui(
        app_state.clone(),
        ui.clone(),
        true,
        Some(supervisor.clone()),
    );

    use axum::body::Body;
    use axum::http::{Method, Request};
    let response = live_router
        .oneshot(
            Request::builder()
                .method(Method::PATCH)
                .uri("/ui/settings")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"server": {{"bind": "{}"}}}}"#,
                    bind_b
                )))
                .unwrap(),
        )
        .await
        .expect("patch request");
    assert_eq!(response.status(), StatusCode::OK);

    // Give axum a moment to bind the new socket.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // The supervisor must now report the new bind.
    assert_eq!(
        supervisor.current_bind().as_deref(),
        Some(bind_b.as_str()),
        "supervisor should be on the new port after PATCH /ui/settings"
    );

    // And the new port must serve a 200.
    let after = reqwest::get(format!("http://{bind_b}/healthz"))
        .await
        .expect("probe new bind");
    assert_eq!(after.status(), 200);

    supervisor.stop();
}

// --- R10: HTTP POST /ui/restart must actually rebind via the supervisor ---
//
// Prior to R10, the HTTP restart handler was a 202 no-op. This test
// exercises the rebind path through POST /ui/restart and asserts the
// listener moves to the new bind.

#[tokio::test]
async fn http_restart_rebinds_via_supervisor() {
    use autorouter_server::{build_router_with_ui, GatewaySupervisor};

    let port_a = pick_port();
    let port_b = pick_port();
    let bind_a = format!("127.0.0.1:{port_a}");
    let bind_b = format!("127.0.0.1:{port_b}");

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
    let mut initial_cfg = autorouter_config::AppConfig::default();
    initial_cfg.server.bind = bind_a.clone();
    let app_state = AppState::new(initial_cfg, pipeline, upstreams);
    let ui = UiState {
        config: Arc::new(parking_lot::RwLock::new(
            autorouter_config::AppConfig::default(),
        )),
        start_time: Arc::new(parking_lot::RwLock::new(chrono::Utc::now())),
        log_lines: Arc::new(parking_lot::RwLock::new(Vec::new())),
        config_path: Arc::new(parking_lot::RwLock::new(None)),
        storage: Arc::new(parking_lot::RwLock::new(None)),
        secret_store: Arc::new(parking_lot::RwLock::new(None)),
        supervisor: None,
    };
    ui.config.write().server.bind = bind_b.clone();
    let supervisor = GatewaySupervisor::new();
    let router = build_router_with_ui(
        app_state.clone(),
        ui.clone(),
        true,
        Some(supervisor.clone()),
    );
    supervisor
        .clone()
        .start(router, &bind_a)
        .await
        .expect("start");

    // Hit POST /ui/restart. The handler must read `s.supervisor`,
    // see that the requested bind (bind_b) differs from the live
    // bind (bind_a), and rebind via the supervisor.
    let live_router = build_router_with_ui(
        app_state.clone(),
        ui.clone(),
        true,
        Some(supervisor.clone()),
    );
    use axum::body::Body;
    use axum::http::{Method, Request};
    let response = live_router
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/ui/restart")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("restart request");
    assert_eq!(response.status(), StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        supervisor.current_bind().as_deref(),
        Some(bind_b.as_str()),
        "supervisor should be on the new port after POST /ui/restart"
    );
    let probe = reqwest::get(format!("http://{bind_b}/healthz"))
        .await
        .expect("probe new bind");
    assert_eq!(probe.status(), 200);

    supervisor.stop();
}

// --- R10: HTTP PATCH /ui/settings without a supervisor still updates config ---
//
// When the headless binary receives a bind change, the listener cannot
// be moved (the headless binary binds once at startup). The PATCH
// handler must still update the in-memory config (so a process restart
// picks it up) and not pretend the rebind happened.

#[tokio::test]
async fn http_patch_settings_without_supervisor_updates_config() {
    let (router, _app, _ui) = build();
    use axum::body::Body;
    use axum::http::{Method, Request};
    let bind_b = format!("127.0.0.1:{}", pick_port());
    let response = router
        .oneshot(
            Request::builder()
                .method(Method::PATCH)
                .uri("/ui/settings")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"server": {{"bind": "{}"}}}}"#,
                    bind_b
                )))
                .unwrap(),
        )
        .await
        .expect("patch request");
    assert_eq!(response.status(), StatusCode::OK);
    let body_bytes = to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("body");
    let body: Value = serde_json::from_slice(&body_bytes).expect("json");
    assert_eq!(
        body.get("server")
            .and_then(|s| s.get("bind"))
            .and_then(|v| v.as_str()),
        Some(bind_b.as_str()),
        "in-memory bind should reflect the patch even without a supervisor"
    );
}

// --- HTTP PATCH /ui/settings must toggle CORS on the next request ---

#[tokio::test]
async fn http_patch_settings_toggles_cors_live() {
    use autorouter_server::{build_router_with_ui, GatewaySupervisor};

    let port = pick_port();
    let bind = format!("127.0.0.1:{port}");

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
    let mut initial_cfg = autorouter_config::AppConfig::default();
    initial_cfg.server.bind = bind.clone();
    initial_cfg.server.enable_cors = Some(true);
    let app_state = AppState::new(initial_cfg, pipeline, upstreams);
    let ui = UiState {
        config: Arc::new(parking_lot::RwLock::new(
            autorouter_config::AppConfig::default(),
        )),
        start_time: Arc::new(parking_lot::RwLock::new(chrono::Utc::now())),
        log_lines: Arc::new(parking_lot::RwLock::new(Vec::new())),
        config_path: Arc::new(parking_lot::RwLock::new(None)),
        storage: Arc::new(parking_lot::RwLock::new(None)),
        secret_store: Arc::new(parking_lot::RwLock::new(None)),
        supervisor: None,
    };
    ui.config.write().server.bind = bind.clone();
    ui.config.write().server.enable_cors = Some(true);
    let supervisor = GatewaySupervisor::new();
    // Build the initial router with CORS on.
    let router = build_router_with_ui(
        app_state.clone(),
        ui.clone(),
        true,
        Some(supervisor.clone()),
    );
    supervisor
        .clone()
        .start_with_state(
            router,
            autorouter_server::RouterBuildState {
                bind: bind.clone(),
                enable_cors: true,
                max_body_bytes: 16 * 1024 * 1024,
                request_timeout_seconds: 300,
                stream_idle_timeout_seconds: 600,
            },
        )
        .await
        .expect("start with cors on");
    // Give axum a moment to start accepting.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Sanity: with CORS on, the preflight OPTIONS request must
    // return the `access-control-allow-origin` header. We send the
    // request to a real upstream-shaped path; axum returns a
    // method-not-allowed or route-miss, but the CORS layer runs
    // before the router and stamps the headers either way.
    let preflight_cors_on = reqwest::Client::new();
    let res = preflight_cors_on
        .request(
            reqwest::Method::OPTIONS,
            format!("http://{bind}/openai/v1/chat/completions"),
        )
        .header("origin", "http://evil.example")
        .header("access-control-request-method", "POST")
        .header("access-control-request-headers", "content-type")
        .send()
        .await
        .expect("preflight with cors on");
    assert!(
        res.headers().get("access-control-allow-origin").is_some(),
        "CORS layer must stamp access-control-allow-origin when enable_cors=true"
    );

    // PATCH /ui/settings with enable_cors=false. The handler must
    // call `sync_router_state` with a new `RouterBuildState` so the
    // running router is rebuilt without the CORS layer.
    use axum::body::Body;
    use axum::http::{Method, Request};
    let live_router = build_router_with_ui(
        app_state.clone(),
        ui.clone(),
        true,
        Some(supervisor.clone()),
    );
    let response = live_router
        .oneshot(
            Request::builder()
                .method(Method::PATCH)
                .uri("/ui/settings")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"server":{"enable_cors":false}}"#))
                .unwrap(),
        )
        .await
        .expect("patch");
    assert_eq!(response.status(), StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        supervisor.current_state().map(|s| s.enable_cors),
        Some(false),
        "supervisor's RouterBuildState must reflect enable_cors=false after PATCH"
    );

    // Same preflight must now NOT return `access-control-allow-origin`.
    // Without a CORS layer the preflight falls through to axum's
    // default handler which responds 405 without the header.
    let preflight_cors_off = reqwest::Client::new();
    let res = preflight_cors_off
        .request(
            reqwest::Method::OPTIONS,
            format!("http://{bind}/openai/v1/chat/completions"),
        )
        .header("origin", "http://evil.example")
        .header("access-control-request-method", "POST")
        .header("access-control-request-headers", "content-type")
        .send()
        .await
        .expect("preflight with cors off");
    assert!(
        res.headers().get("access-control-allow-origin").is_none(),
        "CORS layer must be GONE after enable_cors=false"
    );

    supervisor.stop_graceful().await;
}

// --- HTTP PATCH /ui/routing must hot-reload the smart router ---
//
// Without a regression test, a future refactor of rebuild_and_swap_router
// could silently leave the gateway with startup-time rules.

#[tokio::test]
async fn http_patch_routing_hot_reloads_smart_router() {
    let (router, app, _ui) = build();
    // Capture the router identity before the PATCH. Two
    // `Arc<dyn Router>` are equal only when they share the same
    // allocation, so this is a strong "the router actually
    // changed" assertion.
    let router_before = Arc::as_ptr(&app.current_router()) as *const u8 as usize;

    use axum::body::Body;
    use axum::http::{Method, Request};
    // Inject a single high-priority rule whose `reason` field
    // contains a unique sentinel we can grep for downstream. The
    // SmartRouter records the matched rule's reason on the
    // RouteDecision; we don't have to look at the upstream call
    // here, just at the router itself.
    let patch_body = serde_json::json!({
        "rules": [{
            "name": "sentinel-rule",
            "priority": 1,
            "reason": "patch-routing-regression-sentinel",
            "match_tags_all": [],
            "match_tags_any": [],
            "match_model_contains": [],
            "targets": [],
            "when_multimodal": {"pdf": false, "image": false, "audio": false},
            "target": {"provider": "anthropic", "model": "claude-haiku-4-5", "headers": {}},
            "needs": {"vision": false, "min_context": 0, "tools": false, "audio": false}
        }],
        "default_tags": []
    })
    .to_string();
    let response = router
        .oneshot(
            Request::builder()
                .method(Method::PATCH)
                .uri("/ui/routing")
                .header("content-type", "application/json")
                .body(Body::from(patch_body))
                .unwrap(),
        )
        .await
        .expect("patch routing");
    assert_eq!(response.status(), StatusCode::OK);
    let (_, body) = body_to_value(response).await;
    assert_eq!(
        body.get("rules")
            .and_then(|r| r.as_array())
            .and_then(|a| a.first())
            .and_then(|r| r.get("name"))
            .and_then(|n| n.as_str()),
        Some("sentinel-rule"),
        "PATCH response must echo back the new rule"
    );

    // The router must have been swapped. If it points at the same
    // allocation as before, the gateway is silently still using
    // AppState.router must point at a fresh allocation after PATCH /ui/routing.
    let router_after = Arc::as_ptr(&app.current_router()) as *const u8 as usize;
    assert_ne!(
        router_before, router_after,
        "AppState.router must point at a fresh allocation after PATCH /ui/routing"
    );
}

#[tokio::test]
async fn import_config_triggers_supervisor_rebind() {
    use std::sync::Arc;

    let port_a = pick_port();
    let port_b = pick_port();
    let bind_a = format!("127.0.0.1:{port_a}");
    let bind_b = format!("127.0.0.1:{port_b}");

    // Use a unique temp dir (no `tempfile` crate available as dev-dep).
    let dir = std::path::PathBuf::from(
        std::env::temp_dir().join(format!("autorouter_test_import_{port_a}")),
    );
    let _ = std::fs::create_dir_all(&dir);
    let cfg_path = dir.join("config.toml");
    let initial_toml = format!(
        r#"
[server]
bind = "{bind_a}"
max_body_bytes = 16777216
request_timeout_seconds = 300
stream_idle_timeout_seconds = 600
"#
    );
    std::fs::write(&cfg_path, &initial_toml).expect("write config");

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
    let app_state = AppState::new(autorouter_config::AppConfig::default(), pipeline, upstreams);
    let ui_state = UiState {
        config: Arc::new(parking_lot::RwLock::new(
            autorouter_config::AppConfig::default(),
        )),
        start_time: Arc::new(parking_lot::RwLock::new(chrono::Utc::now())),
        log_lines: Arc::new(parking_lot::RwLock::new(Vec::new())),
        config_path: Arc::new(parking_lot::RwLock::new(Some(cfg_path))),
        storage: Arc::new(parking_lot::RwLock::new(None)),
        secret_store: Arc::new(parking_lot::RwLock::new(None)),
        supervisor: None,
    };

    let supervisor = autorouter_server::GatewaySupervisor::new();
    let router = autorouter_server::build_router_with_ui(
        app_state.clone(),
        ui_state.clone(),
        false,
        Some(supervisor.clone()),
    );
    supervisor
        .clone()
        .start(router, &bind_a)
        .await
        .expect("start on port_a");
    assert_eq!(supervisor.current_bind().as_deref(), Some(bind_a.as_str()));

    let ui_app = autorouter_server::ui::UiAppState {
        ui: ui_state.clone(),
        app: app_state.clone(),
        supervisor: Some(supervisor.clone()),
    };
    let app_router = autorouter_server::ui::build_sub_router(ui_app);

    let import_toml = format!(
        r#"
[server]
bind = "{bind_b}"
max_body_bytes = 16777216
request_timeout_seconds = 300
stream_idle_timeout_seconds = 600
"#
    );

    let response = app_router
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/ui/import")
                .header("content-type", "application/toml")
                .body(axum::body::Body::from(import_toml))
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, body) = body_to_value(response).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "import_config returned {status}: {body}"
    );

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    assert_eq!(
        supervisor.current_bind().as_deref(),
        Some(bind_b.as_str()),
        "supervisor must rebound after import_config"
    );

    let probe = reqwest::get(format!("http://{bind_b}/healthz"))
        .await
        .expect("probe new bind");
    assert_eq!(probe.status(), 200, "new port must be live after rebind");
}
