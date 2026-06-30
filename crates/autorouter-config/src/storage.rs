//! SQLite-backed persistent storage for runtime state.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::error::{ConfigError, ConfigResult};

/// Bumped whenever the SQLite schema changes shape. The
/// `apply_migration` function dispatches the next version in the
/// sequence; every addition is additive (ALTER TABLE ADD COLUMN)
/// so older databases upgrade in place.
pub const SCHEMA_VERSION: u32 = 4;

#[derive(Debug)]
pub struct Storage {
    conn: Mutex<Connection>,
    path: std::path::PathBuf,
}

impl Storage {
    pub fn open(path: impl AsRef<Path>) -> ConfigResult<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&path)?;
        let storage = Self {
            conn: Mutex::new(conn),
            path,
        };
        storage.migrate()?;
        Ok(storage)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn migrate(&self) -> ConfigResult<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ConfigError::Storage(e.to_string()))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER PRIMARY KEY NOT NULL);",
        )?;
        let current: u32 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_version",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        for v in (current + 1)..=SCHEMA_VERSION {
            tracing::info!(version = v, "applying migration");
            conn.execute_batch("BEGIN")?;
            if let Err(e) = apply_migration(&conn, v) {
                conn.execute_batch("ROLLBACK").ok();
                return Err(e);
            }
            if let Err(e) = conn.execute(
                "INSERT INTO schema_version(version) VALUES (?1)",
                params![v],
            ) {
                conn.execute_batch("ROLLBACK").ok();
                return Err(ConfigError::Storage(format!("schema_version insert: {e}")));
            }
            conn.execute_batch("COMMIT")?;
        }
        Ok(())
    }

    pub fn set_setting(&self, key: &str, value: &str) -> ConfigResult<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ConfigError::Storage(e.to_string()))?;
        conn.execute(
            r#"INSERT INTO settings(key, value, updated_at) VALUES (?1, ?2, ?3)
               ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at"#,
            params![key, value, now_seconds()],
        )?;
        Ok(())
    }

    pub fn get_setting(&self, key: &str) -> ConfigResult<Option<String>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ConfigError::Storage(e.to_string()))?;
        let mut stmt = conn.prepare("SELECT value FROM settings WHERE key = ?1")?;
        let mut rows = stmt.query(params![key])?;
        if let Some(row) = rows.next()? {
            Ok(Some(row.get(0)?))
        } else {
            Ok(None)
        }
    }

    pub fn record_provider_event(&self, event: &ProviderEvent) -> ConfigResult<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ConfigError::Storage(e.to_string()))?;
        conn.execute(
            r#"INSERT INTO provider_events
               (provider, model, kind, latency_ms, status, error,
                request_id, session_id, source_provider, created_at,
                input_tokens, output_tokens,
                cache_read_tokens, cache_write_tokens, reasoning_tokens)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                       ?11, ?12, ?13, ?14, ?15)"#,
            params![
                event.provider,
                event.model,
                event.kind,
                event.latency_ms as i64,
                event.status as i64,
                event.error,
                event.request_id,
                event.session_id,
                event.source_provider,
                event.created_at.0 as i64,
                event.input_tokens as i64,
                event.output_tokens as i64,
                event.cache_read_tokens as i64,
                event.cache_write_tokens as i64,
                event.reasoning_tokens as i64,
            ],
        )?;
        Ok(())
    }

    pub fn recent_provider_events(
        &self,
        provider: &str,
        limit: u32,
    ) -> ConfigResult<Vec<ProviderEvent>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ConfigError::Storage(e.to_string()))?;
        let mut stmt = conn.prepare(
            r#"SELECT provider, model, kind, latency_ms, status, error,
                  request_id, session_id, source_provider, created_at,
                  input_tokens, output_tokens,
                  cache_read_tokens, cache_write_tokens, reasoning_tokens
               FROM provider_events
               WHERE provider = ?1
               ORDER BY created_at DESC
               LIMIT ?2"#,
        )?;
        let rows = stmt
            .query_map(params![provider, limit as i64], |row| {
                // Token columns are nullable for pre-v4 rows; treat
                // NULL as 0 so the in-memory `ProviderEvent` keeps
                // the existing semantics of `Default` rather than
                // panicking on a query that returned the legacy
                // 8-column shape.
                let read_opt = |i: usize| -> u64 {
                    row.get::<_, Option<i64>>(i).ok().flatten().unwrap_or(0) as u64
                };
                Ok(ProviderEvent {
                    provider: row.get(0)?,
                    model: row.get(1)?,
                    kind: row.get(2)?,
                    latency_ms: row.get::<_, i64>(3)? as u64,
                    status: row.get::<_, i64>(4)? as u16,
                    error: row.get(5)?,
                    request_id: row.get(6)?,
                    session_id: row.get(7)?,
                    source_provider: row.get(8)?,
                    created_at: UnixSeconds(row.get::<_, i64>(9)? as u64),
                    input_tokens: read_opt(10),
                    output_tokens: read_opt(11),
                    cache_read_tokens: read_opt(12),
                    cache_write_tokens: read_opt(13),
                    reasoning_tokens: read_opt(14),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn upsert_session(
        &self,
        id: &str,
        label: Option<&str>,
        source_provider: &str,
        last_request_id: Option<&str>,
        request_count: u64,
    ) -> ConfigResult<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ConfigError::Storage(e.to_string()))?;
        let now = now_seconds();
        conn.execute(
            r#"INSERT INTO sessions
               (id, label, source_provider, last_request_id, request_count, created_at, updated_at)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
               ON CONFLICT(id) DO UPDATE SET
                 label = excluded.label,
                 source_provider = excluded.source_provider,
                 last_request_id = excluded.last_request_id,
                 request_count = excluded.request_count,
                 updated_at = excluded.updated_at"#,
            params![
                id,
                label,
                source_provider,
                last_request_id,
                request_count as i64,
                now
            ],
        )?;
        Ok(())
    }

    pub fn list_sessions(&self, limit: u32) -> ConfigResult<Vec<PersistedSession>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ConfigError::Storage(e.to_string()))?;
        let mut stmt = conn.prepare(
            r#"SELECT id, label, source_provider, last_request_id, request_count, created_at, updated_at
               FROM sessions ORDER BY updated_at DESC LIMIT ?1"#,
        )?;
        let rows = stmt
            .query_map(params![limit as i64], |row| {
                Ok(PersistedSession {
                    id: row.get(0)?,
                    label: row.get(1)?,
                    source_provider: row.get(2)?,
                    last_request_id: row.get(3)?,
                    request_count: row.get::<_, i64>(4)? as u64,
                    created_at: row.get(5)?,
                    updated_at: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// M11: run a quick `PRAGMA integrity_check` on the database.
    /// Returns Ok(()) when the DB is healthy, or a Storage error
    /// with the corruption message from sqlite otherwise.
    pub fn integrity_check(path: &Path) -> ConfigResult<()> {
        let conn = Connection::open(path)?;
        let result: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        if result == "ok" {
            Ok(())
        } else {
            Err(ConfigError::Storage(format!("integrity_check: {result}")))
        }
    }

    pub fn shutdown(&self, backup_path: Option<&Path>) -> ConfigResult<()> {
        if let Some(backup) = backup_path {
            if let Some(parent) = backup.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let conn = self
                .conn
                .lock()
                .map_err(|e| ConfigError::Storage(e.to_string()))?;
            // Use rusqlite's backup API instead of VACUUM INTO with
            // hand-escaped paths to avoid SQL injection.
            let mut dst = Connection::open(backup)
                .map_err(|e| ConfigError::Storage(format!("backup open error: {e}")))?;
            let backup_handle = rusqlite::backup::Backup::new(&conn, &mut dst)
                .map_err(|e| ConfigError::Storage(format!("backup init error: {e}")))?;
            backup_handle
                .run_to_completion(100, std::time::Duration::from_millis(250), None)
                .map_err(|e| ConfigError::Storage(format!("backup failed: {e}")))?;
        }
        Ok(())
    }
}

fn apply_migration(conn: &Connection, version: u32) -> ConfigResult<()> {
    match version {
        1 => {
            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS settings (
                    key TEXT PRIMARY KEY NOT NULL,
                    value TEXT NOT NULL,
                    updated_at INTEGER NOT NULL
                );
                CREATE TABLE IF NOT EXISTS provider_events (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    provider TEXT NOT NULL,
                    model TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    latency_ms INTEGER NOT NULL,
                    status INTEGER NOT NULL,
                    error TEXT,
                    created_at INTEGER NOT NULL
                );
                CREATE INDEX idx_provider_events_provider_created
                    ON provider_events(provider, created_at DESC);
                "#,
            )?;
            Ok(())
        }
        2 => {
            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS sessions (
                    id TEXT PRIMARY KEY NOT NULL,
                    label TEXT,
                    source_provider TEXT NOT NULL,
                    last_request_id TEXT,
                    request_count INTEGER NOT NULL DEFAULT 0,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL
                );
                CREATE INDEX idx_sessions_source
                    ON sessions(source_provider);
                "#,
            )?;
            Ok(())
        }
        3 => {
            // L9: add request/session correlation columns to provider_events.
            // SQLite ALTER TABLE is additive and does not rewrite the table.
            // Each column is added individually: a crash mid-migration that
            // left some columns in place must not prevent the remaining ones
            // from being added. `add_column_if_missing` introspects
            // `PRAGMA table_info` and skips columns that already exist, so a
            // partially-applied migration converges instead of stalling.
            add_column_if_missing(conn, "provider_events", "request_id", "TEXT")?;
            add_column_if_missing(conn, "provider_events", "session_id", "TEXT")?;
            add_column_if_missing(conn, "provider_events", "source_provider", "TEXT")?;
            conn.execute_batch(
                "CREATE INDEX IF NOT EXISTS idx_provider_events_request
                    ON provider_events(request_id);",
            )?;
            Ok(())
        }
        4 => {
            add_column_if_missing(
                conn,
                "provider_events",
                "input_tokens",
                "INTEGER NOT NULL DEFAULT 0",
            )?;
            add_column_if_missing(
                conn,
                "provider_events",
                "output_tokens",
                "INTEGER NOT NULL DEFAULT 0",
            )?;
            add_column_if_missing(
                conn,
                "provider_events",
                "cache_read_tokens",
                "INTEGER NOT NULL DEFAULT 0",
            )?;
            add_column_if_missing(
                conn,
                "provider_events",
                "cache_write_tokens",
                "INTEGER NOT NULL DEFAULT 0",
            )?;
            add_column_if_missing(
                conn,
                "provider_events",
                "reasoning_tokens",
                "INTEGER NOT NULL DEFAULT 0",
            )?;
            conn.execute_batch(
                "CREATE INDEX IF NOT EXISTS idx_provider_events_created
                    ON provider_events(created_at DESC);",
            )?;
            Ok(())
        }
        other => Err(ConfigError::Storage(format!(
            "no migration registered for schema version {other}"
        ))),
    }
}

/// Add a column to a table only if it is not already present.
///
/// This introspects `PRAGMA table_info(<table>)` so a partially-applied
/// migration (e.g. some columns added before a crash) converges instead of
/// failing with "duplicate column name". This is safer than running a batch
/// of `ALTER TABLE ADD COLUMN` statements: a batch aborts on the first
/// duplicate and leaves the remaining columns missing while the migration
/// is reported as successful.
fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    decl: &str,
) -> ConfigResult<()> {
    // Validate identifiers FIRST — before any SQL is constructed or executed —
    // so injection-unsafe input is rejected even on the PRAGMA path.
    // The `decl` parameter intentionally skips this check because it is a type
    // declaration that may contain spaces and keywords (e.g.
    // "INTEGER NOT NULL DEFAULT 0"); all call sites pass string literals.
    fn safe_ident(s: &str) -> bool {
        s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    }
    if !safe_ident(table) || !safe_ident(column) {
        return Err(ConfigError::Storage(format!(
            "unsafe identifier in add_column_if_missing: {table}.{column}"
        )));
    }
    let pragma = format!("PRAGMA table_info({table})");
    let mut stmt = conn.prepare(&pragma)?;
    let rows = stmt.query_map([], |row| {
        let name: String = row.get(1)?;
        Ok(name)
    })?;
    for res in rows {
        let existing = res?;
        if existing.eq_ignore_ascii_case(column) {
            return Ok(());
        }
    }
    let sql = format!("ALTER TABLE {table} ADD COLUMN {column} {decl}");
    conn.execute(&sql, [])
        .map_err(|e| ConfigError::Storage(e.to_string()))?;
    Ok(())
}

/// Unix epoch seconds. A typed timestamp prevents unit-mix bugs
/// in tests and event handlers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UnixSeconds(pub u64);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderEvent {
    pub provider: String,
    pub model: String,
    pub kind: String,
    pub latency_ms: u64,
    pub status: u16,
    pub error: Option<String>,
    /// L9: request/session correlation. The gateway populates these
    /// from the request headers / RequestContext.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_provider: Option<String>,
    /// L10: typed timestamp.
    pub created_at: UnixSeconds,
    /// Token accounting pulled from the upstream `usage` block. The
    /// schema migration v4 added these columns to `provider_events`
    /// so the dashboard can show real numbers instead of always
    /// rendering 0. Defaults to 0 when the upstream did not report
    /// usage (e.g. early mock implementations, partial failures).
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: u64,
}

impl ProviderEvent {
    /// L7: renamed to `new` so the name does not lie when the
    /// caller is about to override `status` for a non-2xx outcome.
    /// The `success` constructor is kept as a deprecated alias for
    /// backward compatibility with the test fixtures and the
    /// `routes::record_storage_event` shortcut.
    pub fn new(
        provider: impl Into<String>,
        model: impl Into<String>,
        kind: impl Into<String>,
        latency_ms: u64,
    ) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            kind: kind.into(),
            latency_ms,
            status: 200,
            error: None,
            request_id: None,
            session_id: None,
            source_provider: None,
            created_at: UnixSeconds(now_seconds()),
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
        }
    }

    /// Build a `ProviderEvent` whose token fields are populated
    /// from a parsed [`Usage`](autorouter_core::Usage) value. The
    /// caller still owns `provider`, `model`, `kind`, `latency_ms`,
    /// `status`, `error`, `request_id`, `session_id`,
    /// `source_provider`, and `created_at`.
    ///
    /// Missing `Option<u64>` slots on the breakdown collapse to 0;
    /// a `Usage` with all fields `None` produces a row that looks
    /// the same as the legacy `new(...)` shape, so older
    /// fixtures keep working unchanged.
    pub fn with_usage(
        provider: impl Into<String>,
        model: impl Into<String>,
        kind: impl Into<String>,
        latency_ms: u64,
        usage: &autorouter_core::Usage,
    ) -> Self {
        let mut event = Self::new(provider, model, kind, latency_ms);
        event.input_tokens = usage.tokens.input.unwrap_or(0);
        event.output_tokens = usage.tokens.output.unwrap_or(0);
        event.cache_read_tokens = usage.tokens.cache_read.unwrap_or(0);
        event.cache_write_tokens = usage.tokens.cache_write.unwrap_or(0);
        event.reasoning_tokens = usage.tokens.reasoning.unwrap_or(0);
        event
    }

    /// Deprecated: use [`ProviderEvent::new`] instead. Kept so the
    /// few test fixtures that still call `success` keep compiling.
    #[deprecated(note = "renamed to `ProviderEvent::new`")]
    pub fn success(
        provider: impl Into<String>,
        model: impl Into<String>,
        kind: impl Into<String>,
        latency_ms: u64,
    ) -> Self {
        Self::new(provider, model, kind, latency_ms)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedSession {
    pub id: String,
    pub label: Option<String>,
    pub source_provider: String,
    pub last_request_id: Option<String>,
    pub request_count: u64,
    pub created_at: u64,
    pub updated_at: u64,
}

/// L4: exported so other modules in the crate can share it.
pub fn now_seconds() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
