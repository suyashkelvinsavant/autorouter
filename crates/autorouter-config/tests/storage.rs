//! Tests for the SQLite-backed storage.

use autorouter_config::{ProviderEvent, Storage};
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
