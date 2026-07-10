//! End-to-end tests for the auto-route-to-custom behaviour and the
//! provider-label fix.
//!
//! Background — what this file is guarding against:
//!
//! 1. Before this fix, when the operator's `defaults.default_provider`
//!    pointed at a built-in slot (e.g. `openai`) they had not
//!    actually configured, every request fell through to that slot's
//!    `MockUpstream`. The wire response carried `model: "gpt-5"` (or
//!    the configured default), the dashboard recorded it as a real
//!    `openai/gpt-5` request, and an operator who only had OpenRouter
//!    configured saw phantom `gpt-5` rows on the Requests page even
//!    though they had never enabled gpt-5. The fix is
//!    `maybe_route_unconfigured_built_in_to_custom`: when the
//!    default rule picks an unconfigured built-in slot and a custom
//!    provider's `model_allowlist` accepts the request model, the
//!    gateway rewrites the routing decision to that custom provider
//!    and surfaces the real model the user typed.
//!
//! 2. Before this fix, every `provider_events` row for a custom
//!    provider was tagged `provider = "custom"` (the generic bucket)
//!    because `record_storage_event` used
//!    `decision.target_provider.to_string()`. The dashboard's
//!    Requests page grouped all custom providers together, hiding
//!    which one actually served the request. The fix is
//!    `provider_label` — it returns the `custom_target` name when
//!    the kind is `ProviderKind::Custom`.
//!
//! 3. Before this fix, the `/ui/events` endpoint queried
//!    `recent_provider_events` only for the hardcoded
//!    `["openai", "anthropic", "gemini"]` provider ids, so any custom
//!    provider event was invisible. The fix pulls in every
//!    configured custom provider name.
//!
//! All tests inject `MockUpstream` instances for the custom providers
//! so the routing decisions can be exercised without making real
//! network calls.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use parking_lot::RwLock;
use serde_json::{json, Value};
use tower::ServiceExt;

use autorouter_config::ProviderEntry;
use autorouter_core::ProviderKind;
use autorouter_server::{
    AppState, GatewaySupervisor, MockUpstream, SharedUpstream, StorageHandle, TranslationPipeline,
    UiState,
};
use autorouter_translate::{
    AnthropicAdapter, GeminiAdapter, OpenAiChatAdapter, OpenAiResponsesAdapter,
};

fn build_pipeline() -> TranslationPipeline {
    TranslationPipeline::new()
        .register(Arc::new(OpenAiChatAdapter::new()))
        .register(Arc::new(OpenAiResponsesAdapter::new()))
        .register(Arc::new(AnthropicAdapter::new()))
        .register(Arc::new(GeminiAdapter::new()))
}

fn build_state_with_mock_built_ins() -> AppState {
    let pipeline = build_pipeline();
    let mut upstreams: HashMap<ProviderKind, SharedUpstream> = HashMap::new();
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

/// Inject a `MockUpstream` for every custom provider named in the
/// supplied map so the gateway routes to a controllable stub instead
/// of a real HTTP client. Mirrors what the production
/// `build_upstreams` does for configured custom slots, but swaps in
/// the mock implementation we use everywhere else in tests.
fn install_mock_custom_upstreams(state: &AppState, custom: BTreeMap<String, SharedUpstream>) {
    state.upstreams.write().custom = custom;
}

async fn read_body(response: axum::response::Response) -> (StatusCode, String) {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

fn tempdir() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let base = std::env::temp_dir();
    let unique = format!(
        "autorouter-auto-route-test-{}-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
    );
    let dir = base.join(unique);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn make_openrouter_entry() -> ProviderEntry {
    ProviderEntry {
        display_name: "OpenRouter".to_string(),
        base_url: "https://openrouter.ai/api/v1".to_string(),
        enabled: true,
        api_key_secret_id: None,
        default_headers: Default::default(),
        model_allowlist: vec!["nvidia/nemotron-3-ultra-550b-a55b:free".to_string()],
        api_format: autorouter_config::ApiFormat::OpenAI,
    }
}

/// Reproduces the user's exact scenario: the operator registered a
/// custom OpenRouter provider with one model on its allowlist, the
/// configured default still points at the unconfigured built-in
/// OpenAI slot (the previous installer left that as the system
/// default), and a request for the configured model arrives.
///
/// Expected behaviour after the fix:
///   * The wire body sent upstream carries the real model id the
///     user typed (`nvidia/nemotron-3-ultra-550b-a55b:free`), NOT
///     the spurious `gpt-5`.
///   * The `provider_events` row is tagged with the real custom
///     provider name (`openrouter`), NOT the generic `custom` bucket.
///   * The `/ui/events` endpoint surfaces the event (it would have
///     hidden it before, because it only queried the three built-in
///     provider ids).
#[tokio::test]
async fn unconfigured_built_in_default_auto_routes_to_custom_provider() {
    let tmp = tempdir();
    let db = tmp.join("autorouter.db");
    let storage = Arc::new(StorageHandle::open(db).expect("open storage"));
    let state = build_state_with_mock_built_ins().with_storage(Some(storage.clone()));

    // The installed AppConfig defaults the operator never changed.
    let mut config = state.current_config().as_ref().clone();
    config.defaults.default_provider = "openai".to_string();
    config.defaults.default_model = "gpt-5".to_string();
    // No `providers.openai` entry — the operator never configured it.
    // They only configured the custom OpenRouter slot.
    config.providers.openai = None;
    config
        .providers
        .custom
        .insert("openrouter".to_string(), make_openrouter_entry());

    // Replace the custom upstream with a controllable mock so the
    // test does not hit the real OpenRouter endpoint.
    let mut custom = BTreeMap::new();
    custom.insert(
        "openrouter".to_string(),
        Arc::new(MockUpstream::new(ProviderKind::Custom)) as SharedUpstream,
    );
    install_mock_custom_upstreams(&state, custom);

    state.replace_config(config.clone());

    let app = autorouter_server::build_router(state.clone());

    let body = json!({
        "model": "nvidia/nemotron-3-ultra-550b-a55b:free",
        "messages": [{ "role": "user", "content": "hi" }],
    });
    let (status, resp_body) = read_body(
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
    assert_eq!(status, StatusCode::OK, "body: {resp_body}");

    // The response model should be the one the user actually asked
    // for, not the phantom `gpt-5` from the built-in default.
    let v: Value = serde_json::from_str(&resp_body).unwrap();
    assert_eq!(
        v["model"], "nvidia/nemotron-3-ultra-550b-a55b:free",
        "response model should reflect the request, not the stale default"
    );

    // The storage layer must have recorded the event against the
    // real custom provider name (`openrouter`), not the generic
    // `custom` bucket. This is what the Requests page renders.
    let events = storage
        .recent_provider_events("openrouter", 10)
        .expect("read openrouter events");
    assert!(
        !events.is_empty(),
        "expected a provider event tagged with the real custom provider name"
    );
    let last = events.first().unwrap();
    assert_eq!(last.provider, "openrouter");
    assert_eq!(last.model, "nvidia/nemotron-3-ultra-550b-a55b:free");
    assert_eq!(last.status, 200);

    // And there must NOT be a phantom `openai/gpt-5` row sitting in
    // storage from this request. The previous behaviour silently
    // logged the auto-route-to-mock call as if it were a real
    // upstream request.
    let openai_events = storage
        .recent_provider_events("openai", 10)
        .expect("read openai events");
    assert!(
        openai_events.is_empty(),
        "auto-route must not produce a phantom openai event"
    );
}

/// Companion to the above test: when the operator has explicitly
/// configured the built-in OpenAI slot (with a base URL and an API
/// key) the auto-route must NOT redirect traffic away from it.
/// The gateway should respect the operator's explicit choice; the
/// failover chain can still pick the custom provider on retryable
/// upstream failure.
#[tokio::test]
async fn configured_built_in_default_is_not_overridden_by_auto_route() {
    let tmp = tempdir();
    let db = tmp.join("autorouter.db");
    let storage = Arc::new(StorageHandle::open(db).expect("open storage"));
    let state = build_state_with_mock_built_ins().with_storage(Some(storage.clone()));

    let mut config = state.current_config().as_ref().clone();
    config.defaults.default_provider = "openai".to_string();
    config.defaults.default_model = "gpt-5".to_string();
    // Real OpenAI slot configured by the operator.
    config.providers.openai = Some(ProviderEntry {
        display_name: "OpenAI".to_string(),
        base_url: "https://api.openai.com/v1".to_string(),
        enabled: true,
        api_key_secret_id: None,
        default_headers: Default::default(),
        model_allowlist: Vec::new(),
        api_format: autorouter_config::ApiFormat::OpenAI,
    });
    // OpenRouter also configured but with a different allowlist.
    config.providers.custom.insert(
        "openrouter".to_string(),
        ProviderEntry {
            display_name: "OpenRouter".to_string(),
            base_url: "https://openrouter.ai/api/v1".to_string(),
            enabled: true,
            api_key_secret_id: None,
            default_headers: Default::default(),
            model_allowlist: vec!["nvidia/nemotron-3-ultra-550b-a55b:free".to_string()],
            api_format: autorouter_config::ApiFormat::OpenAI,
        },
    );

    // Inject mocks for both built-in (already mocked) and the
    // custom provider so the test never hits the network.
    let mut custom = BTreeMap::new();
    custom.insert(
        "openrouter".to_string(),
        Arc::new(MockUpstream::new(ProviderKind::Custom)) as SharedUpstream,
    );
    install_mock_custom_upstreams(&state, custom);

    state.replace_config(config.clone());

    let app = autorouter_server::build_router(state.clone());

    // The user asks for a model that matches the OpenRouter
    // allowlist, but the operator explicitly configured OpenAI for
    // the default. The auto-route must NOT silently move the call.
    let body = json!({
        "model": "nvidia/nemotron-3-ultra-550b-a55b:free",
        "messages": [{ "role": "user", "content": "hi" }],
    });
    let (status, _resp_body) = read_body(
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
    assert_eq!(status, StatusCode::OK);

    // The event must be tagged as `openai`, not `openrouter` —
    // the operator pinned the default explicitly.
    let openai_events = storage
        .recent_provider_events("openai", 10)
        .expect("read openai events");
    assert_eq!(openai_events.len(), 1);
    let openrouter_events = storage
        .recent_provider_events("openrouter", 10)
        .expect("read openrouter events");
    assert!(
        openrouter_events.is_empty(),
        "auto-route must not fire when the built-in slot is configured"
    );
}

/// When no custom provider accepts the requested model, the
/// auto-route is a no-op. The request lands on the built-in
/// `MockUpstream` (because the slot is unconfigured). The wire
/// response reports the canonical model id (the MockUpstream
/// echoes whatever body it was sent) and the storage event is
/// tagged with the built-in provider kind. This is the legacy
/// behaviour and must continue to work — the auto-route is opt-in
/// by virtue of having a matching custom provider.
#[tokio::test]
async fn auto_route_does_not_fire_without_a_matching_custom_provider() {
    let tmp = tempdir();
    let db = tmp.join("autorouter.db");
    let storage = Arc::new(StorageHandle::open(db).expect("open storage"));
    let state = build_state_with_mock_built_ins().with_storage(Some(storage.clone()));

    let mut config = state.current_config().as_ref().clone();
    config.defaults.default_provider = "openai".to_string();
    config.defaults.default_model = "gpt-5".to_string();
    config.providers.openai = None;
    // OpenRouter configured but allowlist does NOT include gpt-5.
    config.providers.custom.insert(
        "openrouter".to_string(),
        ProviderEntry {
            display_name: "OpenRouter".to_string(),
            base_url: "https://openrouter.ai/api/v1".to_string(),
            enabled: true,
            api_key_secret_id: None,
            default_headers: Default::default(),
            model_allowlist: vec!["nvidia/nemotron-3-ultra-550b-a55b:free".to_string()],
            api_format: autorouter_config::ApiFormat::OpenAI,
        },
    );

    let mut custom = BTreeMap::new();
    custom.insert(
        "openrouter".to_string(),
        Arc::new(MockUpstream::new(ProviderKind::Custom)) as SharedUpstream,
    );
    install_mock_custom_upstreams(&state, custom);

    state.replace_config(config.clone());

    let app = autorouter_server::build_router(state.clone());

    let body = json!({
        "model": "gpt-5",
        "messages": [{ "role": "user", "content": "hi" }],
    });
    let (status, _resp_body) = read_body(
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
    assert_eq!(status, StatusCode::OK);

    // No custom-provider event because the request matched neither
    // an enabled custom slot nor a configured built-in slot.
    let openrouter_events = storage
        .recent_provider_events("openrouter", 10)
        .expect("read openrouter events");
    assert!(
        openrouter_events.is_empty(),
        "no custom-provider event must be recorded when the request model does not match any custom allowlist"
    );
}

/// The dashboard's `/ui/events` endpoint used to query only the
/// three built-in provider ids. After the fix it must include
/// every configured custom provider name, so events for OpenRouter
/// show up on the Requests page.
#[tokio::test]
async fn ui_events_includes_custom_provider_events() {
    use autorouter_server::ui::UiAppState;

    let tmp = tempdir();
    let db = tmp.join("autorouter.db");
    let storage = Arc::new(StorageHandle::open(db).expect("open storage"));
    let state = build_state_with_mock_built_ins().with_storage(Some(storage.clone()));

    let mut config = state.current_config().as_ref().clone();
    config.defaults.default_provider = "openai".to_string();
    config.defaults.default_model = "gpt-5".to_string();
    config.providers.openai = None;
    config
        .providers
        .custom
        .insert("openrouter".to_string(), make_openrouter_entry());

    let mut custom = BTreeMap::new();
    custom.insert(
        "openrouter".to_string(),
        Arc::new(MockUpstream::new(ProviderKind::Custom)) as SharedUpstream,
    );
    install_mock_custom_upstreams(&state, custom);

    state.replace_config(config.clone());

    // Send one request so the storage layer has something to find.
    let app = autorouter_server::build_router(state.clone());
    let body = json!({
        "model": "nvidia/nemotron-3-ultra-550b-a55b:free",
        "messages": [{ "role": "user", "content": "hi" }],
    });
    let (status, _) = read_body(
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
    assert_eq!(status, StatusCode::OK);

    // Sanity check: the storage has the openrouter event we expect
    // to see on the dashboard.
    let pre = storage
        .recent_provider_events("openrouter", 10)
        .expect("read openrouter events");
    assert_eq!(pre.len(), 1, "one openrouter event should be recorded");

    // Now build the UI router (the dashboard surface) and hit
    // `/ui/events` with no provider filter. The endpoint must pull
    // rows for the configured custom provider, not just the
    // first-class built-in slots.
    let store = autorouter_config::build_secret_store("memory", None);
    let ui_state = UiState {
        config: Arc::new(RwLock::new(config.clone())),
        start_time: Arc::new(RwLock::new(chrono::Utc::now())),
        log_lines: Arc::new(RwLock::new(Vec::new())),
        config_path: Arc::new(RwLock::new(None)),
        storage: Arc::new(RwLock::new(Some(storage.clone()))),
        secret_store: Arc::new(RwLock::new(Some(store))),
        supervisor: Some(GatewaySupervisor::new()),
    };
    let ui_app = UiAppState {
        ui: ui_state,
        app: state.clone(),
        supervisor: Some(GatewaySupervisor::new()),
    };
    let ui_router = autorouter_server::ui::build_sub_router(ui_app);

    let (status, resp) = read_body(
        ui_router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/ui/events?limit=10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {resp}");
    let v: Value = serde_json::from_str(&resp).unwrap();
    let events = v["events"].as_array().expect("events array");
    let providers: Vec<&str> = events
        .iter()
        .map(|e| e["provider"].as_str().unwrap_or(""))
        .collect();
    assert!(
        providers.contains(&"openrouter"),
        "dashboard /ui/events must surface openrouter events alongside the built-in slots; got providers={providers:?}"
    );
}

/// **The bug the user actually hit.** The dashboard's OpenCode
/// snippet at `ui/src/pages/Dashboard.tsx` instructs the operator to
/// configure OpenCode with `model = "autorouter/autorouter"` — the
/// sentinel. Without the sentinel branch in
/// `maybe_route_unconfigured_built_in_to_custom`, the helper would
/// see `request.model = "autorouter/autorouter"`, fail to match any
/// custom allowlist, return without rewriting the decision, and the
/// sentinel would then be resolved to the configured default model
/// (`gpt-5`) by `finalise_target_model`. The call would land on the
/// OpenAI MockUpstream and the Requests page would show a phantom
/// `openai/gpt-5` row — exactly what the user reported.
///
/// After the fix, the sentinel branch picks the first enabled
/// custom provider with a non-empty allowlist and uses its first
/// allowlist entry as the target model, so the request reaches
/// OpenRouter with `model = "nvidia/nemotron-3-ultra-550b-a55b:free"`
/// and is recorded as `provider = "openrouter"`.
#[tokio::test]
async fn sentinel_request_auto_routes_to_first_custom_provider() {
    let tmp = tempdir();
    let db = tmp.join("autorouter.db");
    let storage = Arc::new(StorageHandle::open(db).expect("open storage"));
    let state = build_state_with_mock_built_ins().with_storage(Some(storage.clone()));

    // Mirror the exact defaults a fresh install lands on when the
    // operator never touched the Settings page: default provider
    // still the OpenAI slot, default model still `gpt-5`.
    let mut config = state.current_config().as_ref().clone();
    config.defaults.default_provider = "openai".to_string();
    config.defaults.default_model = "gpt-5".to_string();
    config.providers.openai = None;
    config
        .providers
        .custom
        .insert("openrouter".to_string(), make_openrouter_entry());

    let mut custom = BTreeMap::new();
    custom.insert(
        "openrouter".to_string(),
        Arc::new(MockUpstream::new(ProviderKind::Custom)) as SharedUpstream,
    );
    install_mock_custom_upstreams(&state, custom);

    state.replace_config(config.clone());

    let app = autorouter_server::build_router(state.clone());

    // The body OpenCode sends when the operator follows the
    // dashboard's snippet: `model = "autorouter/autorouter"`.
    let body = json!({
        "model": "autorouter/autorouter",
        "messages": [{ "role": "user", "content": "hi" }],
    });
    let (status, resp_body) = read_body(
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
    assert_eq!(status, StatusCode::OK, "body: {resp_body}");

    // The wire response must carry the real model from the
    // OpenRouter allowlist, NOT the phantom `gpt-5`.
    let v: Value = serde_json::from_str(&resp_body).unwrap();
    assert_eq!(
        v["model"], "nvidia/nemotron-3-ultra-550b-a55b:free",
        "sentinel request must resolve to the first configured custom provider's first allowlist model, not the stale default"
    );

    // Storage must record the event against `openrouter`, and must
    // NOT contain any phantom `openai/gpt-5` row.
    let openrouter_events = storage
        .recent_provider_events("openrouter", 10)
        .expect("read openrouter events");
    assert_eq!(openrouter_events.len(), 1, "one openrouter event expected");
    let last = openrouter_events.first().unwrap();
    assert_eq!(last.provider, "openrouter");
    assert_eq!(last.model, "nvidia/nemotron-3-ultra-550b-a55b:free");
    assert_eq!(last.status, 200);

    let openai_events = storage
        .recent_provider_events("openai", 10)
        .expect("read openai events");
    assert!(
        openai_events.is_empty(),
        "sentinel request must not produce a phantom openai event"
    );
}

/// Variant of the above using the bare sentinel `"autorouter"`
/// (without the `provider/` prefix). Some editors send this form
/// when the operator configures `model = "autorouter"` directly.
/// `is_sentinel_model` accepts both shapes and the helper must too.
#[tokio::test]
async fn bare_sentinel_request_auto_routes_to_first_custom_provider() {
    let tmp = tempdir();
    let db = tmp.join("autorouter.db");
    let storage = Arc::new(StorageHandle::open(db).expect("open storage"));
    let state = build_state_with_mock_built_ins().with_storage(Some(storage.clone()));

    let mut config = state.current_config().as_ref().clone();
    config.defaults.default_provider = "openai".to_string();
    config.defaults.default_model = "gpt-5".to_string();
    config.providers.openai = None;
    config
        .providers
        .custom
        .insert("openrouter".to_string(), make_openrouter_entry());

    let mut custom = BTreeMap::new();
    custom.insert(
        "openrouter".to_string(),
        Arc::new(MockUpstream::new(ProviderKind::Custom)) as SharedUpstream,
    );
    install_mock_custom_upstreams(&state, custom);

    state.replace_config(config.clone());

    let app = autorouter_server::build_router(state.clone());

    let body = json!({
        "model": "autorouter",
        "messages": [{ "role": "user", "content": "hi" }],
    });
    let (status, resp_body) = read_body(
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
    assert_eq!(status, StatusCode::OK, "body: {resp_body}");
    let v: Value = serde_json::from_str(&resp_body).unwrap();
    assert_eq!(
        v["model"], "nvidia/nemotron-3-ultra-550b-a55b:free",
        "bare sentinel must resolve the same way as autorouter/autorouter"
    );

    let openai_events = storage
        .recent_provider_events("openai", 10)
        .expect("read openai events");
    assert!(
        openai_events.is_empty(),
        "bare sentinel request must not produce a phantom openai event"
    );
}
