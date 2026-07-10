//! Crash-recovery helpers.
//!
//! The server runs with a persistent SQLite store. On startup it
//! reads the storage and verifies the schema. On shutdown it backs up
//! the database (when configured) and closes the connection.
//!
//! ## Backup schemes
//!
//! AutoRouter uses **timestamped backups** as the documented
//! user-facing scheme (see `manual.md`):
//!
//! ```text
//! <data_dir>/backups/autorouter.db.<UTC timestamp>
//! ```
//!
//! After writing a new backup, call
//! [`prune_timestamped_backups`] so only the newest `keep` files
//! remain.
//!
//! [`rotate_backup`] is a separate rolling scheme that writes
//! sibling files (`<db>.bak.1` … `<db>.bak.N` next to the live
//! database). Prefer the timestamped scheme for shutdown backups;
//! keep `rotate_backup` for tests and any caller that explicitly
//! wants the rolling sibling layout.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use autorouter_config::StorageConfig;
use autorouter_core::CoreError;

/// Validate that the storage directory is writable. Returns a
/// `CoreError::Internal` with an actionable message if not.
pub fn validate_storage(cfg: &StorageConfig) -> Result<(), CoreError> {
    let path = if cfg.data_dir.is_empty() {
        std::env::var("AUTOROUTER_DATA_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
    } else {
        std::path::PathBuf::from(&cfg.data_dir)
    };
    std::fs::create_dir_all(&path).map_err(|e| CoreError::Internal(format!("data dir: {e}")))?;
    let db_path = path.join(&cfg.database_file);
    // Touch the file to make sure we can write.
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&db_path)
        .map_err(|e| CoreError::Internal(format!("open db file: {e}")))?;
    // M11: open the database to apply any pending schema migration
    // and then run a `PRAGMA integrity_check` so a corrupted or stale
    // DB is caught at boot, not on first write. The migration is run
    // inside `Storage::open`; a missing or unreadable DB is logged
    // but never aborts startup because the storage is optional.
    match autorouter_config::Storage::open(&db_path) {
        Ok(_storage) => match autorouter_config::Storage::integrity_check(&db_path) {
            Ok(()) => Ok(()),
            Err(e) => Err(CoreError::Internal(format!(
                "storage integrity check failed: {e}"
            ))),
        },
        Err(e) => {
            tracing::warn!(error = %e, "storage migration did not run; continuing without persisted state");
            Ok(())
        }
    }
}

/// Rolling sibling-file backup rotation.
///
/// Copies the current db to `<db>.bak.1`, shifts older `.bak.N` to
/// `.bak.N+1` up to `keep` files, and deletes the oldest beyond the
/// limit.
///
/// **Note:** this is *not* the scheme used by the headless / desktop
/// shutdown path. Those write timestamped files under a `backups/`
/// directory and then call [`prune_timestamped_backups`]. Prefer that
/// path for user-facing backups so restore paths match `manual.md`.
pub fn rotate_backup(db_path: &Path, keep: u32) -> std::io::Result<()> {
    if !db_path.exists() {
        return Ok(());
    }
    if keep == 0 {
        // No backups wanted, but still wipe any existing ones.
        let parent = db_path.parent().unwrap_or_else(|| Path::new("."));
        let Some(name) = db_path.file_name().and_then(|name| name.to_str()) else {
            return Ok(());
        };
        let prefix = format!("{name}.bak.");
        for entry in std::fs::read_dir(parent)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let entry_name = entry.file_name();
            let Some(suffix) = entry_name
                .to_str()
                .and_then(|entry_name| entry_name.strip_prefix(&prefix))
            else {
                continue;
            };
            if suffix.parse::<u32>().is_ok() {
                std::fs::remove_file(entry.path())?;
            }
        }
        return Ok(());
    }
    // Delete the oldest beyond the keep limit.
    let overflow = sibling_backup_path(db_path, keep);
    if overflow.exists() {
        let _ = std::fs::remove_file(&overflow);
    }
    // Shift .bak.(N-1) -> .bak.N, ..., .bak.1 -> .bak.2
    for n in (1..keep).rev() {
        let src = sibling_backup_path(db_path, n);
        let dst = sibling_backup_path(db_path, n + 1);
        if src.exists() {
            std::fs::rename(&src, &dst)?;
        }
    }
    // Copy the current db to .bak.1
    let first = sibling_backup_path(db_path, 1);
    std::fs::copy(db_path, &first)?;
    Ok(())
}

fn sibling_backup_path(db_path: &Path, n: u32) -> PathBuf {
    let mut s = db_path.as_os_str().to_os_string();
    s.push(format!(".bak.{}", n));
    PathBuf::from(s)
}

/// Prune timestamped backup files under `backup_dir`.
///
/// Keeps the newest `keep` files whose names start with
/// `{database_file}.` (e.g. `autorouter.db.20260710T120000Z`).
/// Older matching files are deleted. Non-matching entries in the
/// directory are left untouched.
///
/// * `keep == 0` removes every matching backup.
/// * Missing `backup_dir` is a no-op (returns `Ok(0)`).
/// * Individual delete failures are logged via `tracing` and
///   skipped so one locked file does not block the rest.
///
/// Returns the number of files successfully deleted.
pub fn prune_timestamped_backups(
    backup_dir: &Path,
    database_file: &str,
    keep: u32,
) -> std::io::Result<usize> {
    if !backup_dir.exists() {
        return Ok(0);
    }
    if database_file.is_empty() {
        return Ok(0);
    }

    let prefix = format!("{database_file}.");
    // (path, stamp_suffix, mtime) — stamp is preferred for ordering
    // because shutdown names use sortable UTC stamps
    // (`%Y%m%dT%H%M%SZ`); mtime is the fallback when stamps collide.
    let mut candidates: Vec<(PathBuf, String, SystemTime)> = Vec::new();

    for entry in std::fs::read_dir(backup_dir)? {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(error = %e, "skipping unreadable backup dir entry");
                continue;
            }
        };
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        // Match `autorouter.db.<stamp>` but not the bare `autorouter.db`.
        if !name.starts_with(&prefix) || name.len() <= prefix.len() {
            continue;
        }
        let stamp = name[prefix.len()..].to_string();
        let modified = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        candidates.push((path, stamp, modified));
    }

    // Newest first: stamp descending (ISO-like stamps sort lexicographically),
    // then mtime, then path for full determinism.
    candidates.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| b.2.cmp(&a.2))
            .then_with(|| a.0.cmp(&b.0))
    });

    let keep_usize = keep as usize;
    let to_delete = if keep_usize >= candidates.len() {
        &[][..]
    } else {
        &candidates[keep_usize..]
    };

    let mut deleted = 0usize;
    for (path, _, _) in to_delete {
        match std::fs::remove_file(path) {
            Ok(()) => deleted += 1,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to prune old backup"
                );
            }
        }
    }
    Ok(deleted)
}
