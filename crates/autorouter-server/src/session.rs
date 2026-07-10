//! Session and request tracking.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use autorouter_core::{RequestId, SessionId};

/// How often the background pruner wakes up.
pub const DEFAULT_SESSION_PRUNE_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Sessions with no activity for longer than this are eligible for
/// age-based eviction. Uses `updated_at`, not `created_at`, so a
/// long-lived tool that keeps sending traffic is never pruned.
pub const DEFAULT_SESSION_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Soft cap on in-memory sessions. After age-based pruning, if the
/// registry still exceeds this, the oldest-by-`updated_at` sessions
/// are evicted first.
pub const DEFAULT_SESSION_CAP: usize = 10_000;

/// Lightweight session metadata. The gateway keeps an entry per
/// connected tool (Claude Code, Codex, ...).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    /// Free-form label provided by the tool via the
    /// `X-AutoRouter-Session` header.
    pub label: Option<String>,
    /// Source provider advertised by the tool via the
    /// `X-AutoRouter-Source` header.
    pub source_provider: String,
    /// When the session was first observed.
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// When the last request was observed on this session.
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// Last request id seen on this session.
    pub last_request_id: Option<RequestId>,
    /// Total requests observed.
    pub request_count: u64,
}

impl Session {
    /// Construct a fresh session with a freshly-generated id. Used
    /// when no `X-AutoRouter-Session` header is present.
    pub fn new(source_provider: String, label: Option<String>) -> Self {
        Self {
            id: SessionId::new(),
            label,
            source_provider,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            last_request_id: None,
            request_count: 0,
        }
    }

    /// Construct a session with a caller-supplied id. Used when
    /// the client sends a valid `X-AutoRouter-Session: <uuid>`
    /// header — the session id then matches the one the client
    /// already tracks, so subsequent requests on the same header
    /// value update the same row instead of creating a new one
    /// with a generated id.
    pub fn with_id(id: SessionId, source_provider: String, label: Option<String>) -> Self {
        Self {
            id,
            label,
            source_provider,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            last_request_id: None,
            request_count: 0,
        }
    }
}

/// In-memory session registry. Production deployments would persist
/// these via the storage crate; the in-memory form is enough for the
/// Phase 3 MVP.
#[derive(Default, Clone)]
pub struct SessionRegistry {
    inner: Arc<RwLock<std::collections::HashMap<SessionId, Session>>>,
    /// H13: optional storage handle for persisting sessions across restarts.
    storage: Option<Arc<crate::storage::StorageHandle>>,
}

impl SessionRegistry {
    pub fn with_storage(storage: Arc<crate::storage::StorageHandle>) -> Self {
        Self {
            inner: Default::default(),
            storage: Some(storage),
        }
    }

    pub fn new() -> Self {
        Self::default()
    }

    /// Number of sessions currently held in memory.
    pub fn len(&self) -> usize {
        self.inner.read().len()
    }

    /// `true` when the registry holds no sessions.
    pub fn is_empty(&self) -> bool {
        self.inner.read().is_empty()
    }

    /// Look up an existing session by id or create a new one.
    ///
    /// When `id_hint` is `Some(id)`:
    /// 1. If a session with that id already exists in the registry,
    ///    return it (subsequent requests on the same header share
    ///    the same row, with `request_count` incremented by the
    ///    caller via [`record_request`]).
    /// 2. Otherwise construct a new `Session` whose `id` IS the
    ///    supplied `id`, persist it, and return it. The
    ///    `X-AutoRouter-Session` header from the caller is used
    ///    as the session id when present; a fresh UUID is generated
    ///    only when no header is sent.
    pub fn get_or_create(
        &self,
        id_hint: Option<SessionId>,
        source_provider: &str,
        label: Option<String>,
    ) -> Session {
        let mut inner = self.inner.write();
        if let Some(id) = id_hint.as_ref() {
            if let Some(existing) = inner.get(id).cloned() {
                return existing;
            }
        }
        let session = match id_hint {
            Some(id) => Session::with_id(id, source_provider.to_string(), label),
            None => Session::new(source_provider.to_string(), label),
        };
        inner.insert(session.id.clone(), session.clone());
        // H13: persist to storage so the session survives a restart.
        if let Some(storage) = &self.storage {
            let _ = storage.upsert_session(
                &session.id.0.to_string(),
                session.label.as_deref(),
                &session.source_provider,
                session
                    .last_request_id
                    .as_ref()
                    .map(|r| r.0.to_string())
                    .as_deref(),
                session.request_count,
            );
        }
        session
    }

    /// Record a request id and bump the counter on an existing session.
    pub fn record_request(&self, id: &SessionId, request_id: RequestId) {
        let mut guard = self.inner.write();
        if let Some(s) = guard.get_mut(id) {
            s.updated_at = chrono::Utc::now();
            s.last_request_id = Some(request_id);
            s.request_count = s.request_count.saturating_add(1);
            let snap = s.clone();
            drop(guard);
            if let Some(storage) = &self.storage {
                let _ = storage.upsert_session(
                    &snap.id.0.to_string(),
                    snap.label.as_deref(),
                    &snap.source_provider,
                    snap.last_request_id
                        .as_ref()
                        .map(|r| r.0.to_string())
                        .as_deref(),
                    snap.request_count,
                );
            }
        }
    }

    /// Active sessions, newest first.
    pub fn list(&self) -> Vec<Session> {
        let mut v: Vec<Session> = self.inner.read().values().cloned().collect();
        v.sort_by_key(|b| std::cmp::Reverse(b.updated_at));
        v
    }

    /// Remove sessions whose `updated_at` is older than `max_age`, then
    /// enforce a soft cap of `max_sessions` by evicting the least-
    /// recently-active remaining sessions. Returns the total number of
    /// pruned sessions.
    ///
    /// When a storage handle is attached, pruned ids are also deleted
    /// from SQLite, and any orphaned rows whose `updated_at` falls
    /// outside the age window are purged even if they were never
    /// loaded into the in-memory map.
    pub fn prune_older_than(&self, max_age: chrono::Duration, max_sessions: usize) -> usize {
        let cutoff = chrono::Utc::now() - max_age;
        let mut guard = self.inner.write();
        let before = guard.len();

        // 1. Age-based eviction by last activity (not creation).
        let mut removed_ids: Vec<String> = Vec::new();
        guard.retain(|id, s| {
            if s.updated_at >= cutoff {
                true
            } else {
                removed_ids.push(id.0.to_string());
                false
            }
        });

        // 2. Cap-based eviction (least recently active first).
        if guard.len() > max_sessions {
            let mut sessions: Vec<(SessionId, chrono::DateTime<chrono::Utc>)> = guard
                .iter()
                .map(|(id, s)| (id.clone(), s.updated_at))
                .collect();
            sessions.sort_by_key(|(_, ts)| *ts);
            let overflow = guard.len() - max_sessions;
            let to_remove: Vec<SessionId> = sessions
                .into_iter()
                .take(overflow)
                .map(|(id, _)| id)
                .collect();
            for id in &to_remove {
                removed_ids.push(id.0.to_string());
                guard.remove(id);
            }
        }

        let after = guard.len();
        drop(guard);

        // Persist the eviction to SQLite when available.
        if let Some(storage) = &self.storage {
            if !removed_ids.is_empty() {
                let refs: Vec<&str> = removed_ids.iter().map(String::as_str).collect();
                if let Err(e) = storage.delete_sessions(&refs) {
                    tracing::warn!(error = %e, "failed to delete pruned sessions from storage");
                }
            }
            // Also scrub any orphaned SQLite rows that outlived the
            // age window without being present in memory (e.g. after
            // a process restart).
            let cutoff_unix = cutoff.timestamp();
            match storage.prune_sessions_updated_before(cutoff_unix) {
                Ok(n) if n > 0 => {
                    tracing::debug!(
                        sessions_removed = n,
                        "pruned orphaned sessions from storage"
                    );
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "failed to prune orphaned sessions from storage");
                }
            }
            match storage.prune_sessions_to_limit(max_sessions) {
                Ok(n) if n > 0 => {
                    tracing::debug!(sessions_removed = n, "pruned excess sessions from storage");
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "failed to enforce session storage cap");
                }
            }
        }

        before - after
    }
}

/// Helper: derive a stable session id from an optional header value
/// or generate a fresh one.
pub fn session_id_from_header(value: Option<&str>) -> Option<SessionId> {
    value
        .and_then(|v| Uuid::parse_str(v.trim()).ok())
        .map(SessionId::from)
}

/// Async prune loop body. Prefer [`start_session_pruner`] when a
/// Tokio runtime is already entered; use this future with an
/// external spawner (e.g. Tauri's async runtime) otherwise.
pub async fn run_session_pruner(
    registry: SessionRegistry,
    interval: Duration,
    max_age: chrono::Duration,
    max_sessions: usize,
) {
    let mut ticker = tokio::time::interval(interval);
    // Skip bursts after a long stall so we never pile up prune work.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // The first tick completes immediately; wait one full interval
    // before the first prune so startup traffic is not contending
    // with eviction.
    ticker.tick().await;
    loop {
        ticker.tick().await;
        let removed = registry.prune_older_than(max_age, max_sessions);
        if removed > 0 {
            tracing::info!(sessions_removed = removed, "pruned stale sessions");
        }
    }
}

/// Spawn a background Tokio task that periodically prunes stale
/// sessions from the registry. Call once at startup; the returned
/// `JoinHandle` can be used to cancel the task on shutdown.
///
/// * `interval` — how often to run the prune check.
/// * `max_age` — sessions with no activity older than this are removed.
/// * `max_sessions` — soft cap: if the registry exceeds this after
///   age-based pruning, the least-recently-active sessions are
///   evicted.
pub fn start_session_pruner(
    registry: SessionRegistry,
    interval: Duration,
    max_age: chrono::Duration,
    max_sessions: usize,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run_session_pruner(
        registry,
        interval,
        max_age,
        max_sessions,
    ))
}

/// Convenience: start the pruner with production defaults.
pub fn start_session_pruner_default(registry: SessionRegistry) -> tokio::task::JoinHandle<()> {
    let max_age = chrono::Duration::from_std(DEFAULT_SESSION_MAX_AGE)
        .unwrap_or_else(|_| chrono::Duration::hours(24));
    start_session_pruner(
        registry,
        DEFAULT_SESSION_PRUNE_INTERVAL,
        max_age,
        DEFAULT_SESSION_CAP,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_removes_stale_by_updated_at() {
        let reg = SessionRegistry::new();
        let fresh = reg.get_or_create(None, "openai", Some("fresh".into()));
        let stale = reg.get_or_create(None, "openai", Some("stale".into()));

        // Backdate the stale session's activity far into the past.
        {
            let mut guard = reg.inner.write();
            if let Some(s) = guard.get_mut(&stale.id) {
                s.updated_at = chrono::Utc::now() - chrono::Duration::hours(48);
            }
        }

        let removed = reg.prune_older_than(chrono::Duration::hours(24), 10_000);
        assert_eq!(removed, 1);
        let remaining: Vec<_> = reg.list().into_iter().map(|s| s.id).collect();
        assert!(remaining.contains(&fresh.id));
        assert!(!remaining.contains(&stale.id));
    }

    #[test]
    fn prune_enforces_cap_on_least_recent() {
        let reg = SessionRegistry::new();
        let a = reg.get_or_create(None, "openai", Some("a".into()));
        let b = reg.get_or_create(None, "openai", Some("b".into()));
        let c = reg.get_or_create(None, "openai", Some("c".into()));

        {
            let mut guard = reg.inner.write();
            let now = chrono::Utc::now();
            if let Some(s) = guard.get_mut(&a.id) {
                s.updated_at = now - chrono::Duration::hours(3);
            }
            if let Some(s) = guard.get_mut(&b.id) {
                s.updated_at = now - chrono::Duration::hours(2);
            }
            if let Some(s) = guard.get_mut(&c.id) {
                s.updated_at = now - chrono::Duration::hours(1);
            }
        }

        // Cap of 2, max_age large enough that age eviction does nothing.
        let removed = reg.prune_older_than(chrono::Duration::hours(24), 2);
        assert_eq!(removed, 1);
        let remaining: Vec<_> = reg.list().into_iter().map(|s| s.id).collect();
        assert!(!remaining.contains(&a.id), "least-recent must be evicted");
        assert!(remaining.contains(&b.id));
        assert!(remaining.contains(&c.id));
    }

    #[test]
    fn active_session_survives_age_window_from_creation() {
        // A session created long ago but updated recently must stay.
        let reg = SessionRegistry::new();
        let s = reg.get_or_create(None, "openai", Some("long-lived".into()));
        {
            let mut guard = reg.inner.write();
            if let Some(sess) = guard.get_mut(&s.id) {
                sess.created_at = chrono::Utc::now() - chrono::Duration::days(30);
                sess.updated_at = chrono::Utc::now();
            }
        }
        let removed = reg.prune_older_than(chrono::Duration::hours(24), 10_000);
        assert_eq!(removed, 0);
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn list_orders_by_last_activity() {
        let reg = SessionRegistry::new();
        let older = reg.get_or_create(None, "openai", Some("older".into()));
        let newer = reg.get_or_create(None, "openai", Some("newer".into()));
        {
            let mut guard = reg.inner.write();
            let now = chrono::Utc::now();
            guard.get_mut(&older.id).unwrap().updated_at = now - chrono::Duration::hours(1);
            guard.get_mut(&newer.id).unwrap().updated_at = now;
        }

        let ids: Vec<_> = reg.list().into_iter().map(|session| session.id).collect();
        assert_eq!(ids, vec![newer.id, older.id]);
    }
}
