//! Storage handle that wraps `autorouter_config::Storage` so the UI
//! can read provider events and persist settings without taking a
//! dependency on the rusqlite types directly.

use std::sync::Arc;

use autorouter_config::{PersistedSession, ProviderEvent, Storage};
use parking_lot::Mutex;

/// Thread-safe handle to the SQLite store.
#[derive(Clone)]
pub struct StorageHandle {
    inner: Arc<Mutex<Storage>>,
}

impl StorageHandle {
    /// Open the store at the given path.
    pub fn open(path: std::path::PathBuf) -> Result<Self, String> {
        Storage::open(&path)
            .map(|s| Self {
                inner: Arc::new(Mutex::new(s)),
            })
            .map_err(|e| e.to_string())
    }

    /// Persist a settings value under `key`.
    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), String> {
        let guard = self.inner.lock();
        guard.set_setting(key, value).map_err(|e| e.to_string())
    }

    /// Load a settings value by key.
    pub fn get_setting(&self, key: &str) -> Result<Option<String>, String> {
        let guard = self.inner.lock();
        guard.get_setting(key).map_err(|e| e.to_string())
    }

    /// Record a provider event.
    pub fn record_provider_event(&self, event: &ProviderEvent) -> Result<(), String> {
        let guard = self.inner.lock();
        guard
            .record_provider_event(event)
            .map_err(|e| e.to_string())
    }

    /// Recent events for a provider.
    pub fn recent_provider_events(
        &self,
        provider: &str,
        limit: u32,
    ) -> Result<Vec<ProviderEvent>, String> {
        let guard = self.inner.lock();
        guard
            .recent_provider_events(provider, limit)
            .map_err(|e| e.to_string())
    }

    /// Snapshot the on-disk database to `backup_path` (typically
    /// `<data_dir>/backups/autorouter.db.<UTC timestamp>`). The
    /// original SQLite file stays in place so the next launch can
    /// open it again.
    /// H13: persist or update a session row.
    pub fn upsert_session(
        &self,
        id: &str,
        label: Option<&str>,
        source_provider: &str,
        last_request_id: Option<&str>,
        request_count: u64,
    ) -> Result<(), String> {
        let guard = self.inner.lock();
        guard
            .upsert_session(id, label, source_provider, last_request_id, request_count)
            .map_err(|e| e.to_string())
    }

    /// H13: list the most-recently-active persisted sessions.
    pub fn list_sessions(&self, limit: u32) -> Result<Vec<PersistedSession>, String> {
        let guard = self.inner.lock();
        guard.list_sessions(limit).map_err(|e| e.to_string())
    }

    pub fn shutdown(&self, backup_path: Option<&std::path::Path>) -> Result<(), String> {
        let guard = self.inner.lock();
        guard.shutdown(backup_path).map_err(|e| e.to_string())
    }
}
