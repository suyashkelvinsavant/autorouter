//! Session and request tracking.

use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use autorouter_core::{RequestId, SessionId};

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
        v.sort_by_key(|b| std::cmp::Reverse(b.created_at));
        v
    }

    /// Remove sessions whose `created_at` is older than `max_age`, then
    /// enforce a soft cap of `max_sessions` by evicting the oldest
    /// remaining sessions. Returns the total number of pruned sessions.
    pub fn prune_older_than(&self, max_age: chrono::Duration, max_sessions: usize) -> usize {
        let cutoff = chrono::Utc::now() - max_age;
        let mut guard = self.inner.write();
        let before = guard.len();

        // 1. Age-based eviction.
        guard.retain(|_id, s| s.created_at >= cutoff);

        // 2. Cap-based eviction (oldest first).
        if guard.len() > max_sessions {
            let mut sessions: Vec<(SessionId, chrono::DateTime<chrono::Utc>)> = guard
                .iter()
                .map(|(id, s)| (id.clone(), s.created_at))
                .collect();
            sessions.sort_by_key(|(_, ts)| *ts);
            let to_remove: Vec<SessionId> = sessions
                .into_iter()
                .take(guard.len() - max_sessions)
                .map(|(id, _)| id)
                .collect();
            for id in &to_remove {
                guard.remove(id);
            }
        }

        let after = guard.len();
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

/// Spawn a background tokio task that periodically prunes stale
/// sessions from the registry. Call once at startup; the returned
/// `JoinHandle` can be used to cancel the task on shutdown.
///
/// * `interval` — how often to run the prune check.
/// * `max_age` — sessions older than this are removed.
/// * `max_sessions` — soft cap: if the registry exceeds this after
///   age-based pruning, the oldest extra sessions are evicted.
pub fn start_session_pruner(
    registry: Arc<SessionRegistry>,
    interval: std::time::Duration,
    max_age: chrono::Duration,
    max_sessions: usize,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            let removed = registry.prune_older_than(max_age, max_sessions);
            if removed > 0 {
                tracing::info!(sessions_removed = removed, "pruned stale sessions");
            }
        }
    })
}
