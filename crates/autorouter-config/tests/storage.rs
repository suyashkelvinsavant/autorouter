//! Tests for the SQLite-backed storage.

use autorouter_config::{ProviderEvent, Storage};
use rusqlite::Connection;
use tempfile::tempdir;

#[test]
fn opens_and_migrates() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("autorouter.db");
    let s = Storage::open(&path).unwrap();
    assert_eq!(s.path(), path);
}

#[test]
fn settings_round_trip() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("autorouter.db");
    let s = Storage::open(&path).unwrap();
    s.set_setting("last_model", "gpt-5").unwrap();
    let v = s.get_setting("last_model").unwrap();
    assert_eq!(v.as_deref(), Some("gpt-5"));
    let missing = s.get_setting("nope").unwrap();
    assert!(missing.is_none());
    s.set_setting("last_model", "claude-sonnet-4-5").unwrap();
    assert_eq!(
        s.get_setting("last_model").unwrap().as_deref(),
        Some("claude-sonnet-4-5")
    );
}

#[test]
fn records_provider_events() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("autorouter.db");
    let s = Storage::open(&path).unwrap();
    for i in 0..5 {
        s.record_provider_event(&ProviderEvent::new("openai", "gpt-5", "request", 100 + i))
            .unwrap();
    }
    let events = s.recent_provider_events("openai", 3).unwrap();
    assert_eq!(events.len(), 3);
    for w in events.windows(2) {
        assert!(w[0].created_at >= w[1].created_at);
    }
}

#[test]
fn shutdown_creates_backup() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("autorouter.db");
    let backup = dir.path().join("autorouter.db.bak");
    {
        let s = Storage::open(&path).unwrap();
        s.set_setting("k", "v").unwrap();
        s.shutdown(Some(&backup)).unwrap();
    }
    assert!(backup.exists());
    let restored = Storage::open(&backup).unwrap();
    assert_eq!(restored.get_setting("k").unwrap().as_deref(), Some("v"));
}

/// Regression test for H9: if a migration was partially applied (some
/// columns added before a crash), re-running the migration must add the
/// remaining columns rather than aborting on the first "duplicate column".
/// The previous batch-based fix would skip every column after the first
/// duplicate, leaving the schema incomplete.
#[test]
fn migration_converges_after_partial_application() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("autorouter.db");

    // Build a database at schema version 2 and simulate a partial v3
    // migration where only `request_id` was added before a crash.
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE settings (
                key TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE provider_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                provider TEXT NOT NULL,
                model TEXT NOT NULL,
                kind TEXT NOT NULL,
                latency_ms INTEGER NOT NULL,
                status INTEGER NOT NULL,
                error TEXT,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE sessions (
                id TEXT PRIMARY KEY NOT NULL,
                label TEXT,
                source_provider TEXT NOT NULL,
                last_request_id TEXT,
                request_count INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            ALTER TABLE provider_events ADD COLUMN request_id TEXT;
            CREATE TABLE schema_version (version INTEGER PRIMARY KEY NOT NULL);
            INSERT INTO schema_version(version) VALUES (1), (2);
            "#,
        )
        .unwrap();
    }

    // Opening re-runs migrations from version 3. `request_id` already
    // exists; `session_id` and `source_provider` must still be added.
    let s = Storage::open(&path).unwrap();

    // If any v3/v4 column were missing, this insert would fail.
    let mut event = ProviderEvent::new("openai", "gpt-5", "request", 42);
    event.status = 200;
    event.request_id = Some("req_1".into());
    event.session_id = Some("sess_1".into());
    event.source_provider = Some("openai".into());
    event.input_tokens = 10;
    event.output_tokens = 20;
    event.reasoning_tokens = 5;
    s.record_provider_event(&event).unwrap();

    let events = s.recent_provider_events("openai", 10).unwrap();
    assert_eq!(events.len(), 1);
    let e = &events[0];
    assert_eq!(e.session_id.as_deref(), Some("sess_1"));
    assert_eq!(e.source_provider.as_deref(), Some("openai"));
    assert_eq!(e.input_tokens, 10);
    assert_eq!(e.reasoning_tokens, 5);
}
