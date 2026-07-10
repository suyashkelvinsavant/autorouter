//! Integration test for the storage layer in call_upstream.
//!
//! Spins up a real gateway with a tempdir-backed SQLite store, sends
//! a request through `/openai/v1/chat/completions`, and asserts that
//! `record_provider_event` was called for the upstream call.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::json;
use tower::ServiceExt;

use autorouter_core::ProviderKind;
use autorouter_server::{AppState, MockUpstream, StorageHandle, TranslationPipeline};
use autorouter_translate::{
    AnthropicAdapter, GeminiAdapter, OpenAiChatAdapter, OpenAiResponsesAdapter,
};

fn build_state_with_storage(storage: Arc<StorageHandle>) -> AppState {
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
        .with_storage(Some(storage))
}

#[tokio::test]
async fn upstream_call_records_provider_event() {
    let tmp = tempdir();
    let path = tmp.join("autorouter.db");
    let storage = Arc::new(StorageHandle::open(path).expect("open storage"));

    let app = autorouter_server::build_router(build_state_with_storage(storage.clone()));

    let body = json!({
        "model": "gpt-5",
        "messages": [{ "role": "user", "content": "hi" }],
    });
    let (status, _body) = read_body(
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

    let events = storage
        .recent_provider_events("openai", 10)
        .expect("read events");
    assert!(
        !events.is_empty(),
        "expected at least one provider event after a request"
    );
    let last = events.first().unwrap();
    assert_eq!(last.provider, "openai");
    assert_eq!(last.kind, "request");
    assert_eq!(last.status, 200);
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
    // Per-process counter so concurrent tests within the same
    // nanosecond still get distinct paths. The previous
    // implementation used only `timestamp_nanos_opt()`, which
    // could collide when tests ran in parallel; that surfaced as
    // `sqlite error: duplicate column name: request_id` because
    // the second test would re-apply migration v3 onto a DB
    // already migrated by the first.
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let base = std::env::temp_dir();
    let unique = format!(
        "autorouter-test-{}-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
    );
    let dir = base.join(unique);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn storage_handle_shutdown_writes_backup() {
    let tmp = tempdir();
    let db = tmp.join("autorouter.db");
    let storage = StorageHandle::open(db.clone()).expect("open storage");
    storage
        .set_setting("k", "v")
        .expect("set_setting should succeed");
    let backup = tmp.join("backups").join("autorouter.db.test");
    storage.shutdown(Some(&backup)).expect("shutdown");
    assert!(backup.exists(), "backup file should be created");
    // The original DB still has the row.
    let reopened = StorageHandle::open(db).expect("reopen");
    let v = reopened.get_setting("k").expect("get_setting").unwrap();
    assert_eq!(v, "v");
}
