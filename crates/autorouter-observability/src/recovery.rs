//! Crash-recovery helpers.
//!
//! The server runs with a persistent SQLite store. On startup it
//! reads the storage and verifies the schema. On shutdown it backs up
//! the database (when configured) and closes the connection.

use std::path::Path;

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

/// H7: rolling backup rotation. Copies the current db to
/// `<db>.bak.1`, shifts older `.bak.N` to `.bak.N+1` up to `keep`
/// files, and deletes the oldest beyond the limit.
pub fn rotate_backup(db_path: &Path, keep: u32) -> std::io::Result<()> {
    if !db_path.exists() {
        return Ok(());
    }
    if keep == 0 {
        // No backups wanted, but still wipe any existing ones.
        for n in 1..=10u32 {
            let p = backup_path(db_path, n);
            if p.exists() {
                let _ = std::fs::remove_file(&p);
            }
        }
        return Ok(());
    }
    // Delete the oldest beyond the keep limit.
    let overflow = backup_path(db_path, keep);
    if overflow.exists() {
        let _ = std::fs::remove_file(&overflow);
    }
    // Shift .bak.(N-1) -> .bak.N, ..., .bak.1 -> .bak.2
    for n in (1..keep).rev() {
        let src = backup_path(db_path, n);
        let dst = backup_path(db_path, n + 1);
        if src.exists() {
            std::fs::rename(&src, &dst)?;
        }
    }
    // Copy the current db to .bak.1
    let first = backup_path(db_path, 1);
    std::fs::copy(db_path, &first)?;
    Ok(())
}

fn backup_path(db_path: &Path, n: u32) -> std::path::PathBuf {
    let mut s = db_path.as_os_str().to_os_string();
    s.push(format!(".bak.{}", n));
    std::path::PathBuf::from(s)
}
