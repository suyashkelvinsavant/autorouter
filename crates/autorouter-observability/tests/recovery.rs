//! Tests for the recovery helpers.

use std::fs;

use autorouter_config::StorageConfig;
use autorouter_observability::{rotate_backup, validate_storage};
use tempfile::tempdir;

#[test]
fn validate_storage_creates_db() {
    let dir = tempdir().unwrap();
    let cfg = StorageConfig {
        data_dir: dir.path().to_string_lossy().to_string(),
        database_file: "autorouter.db".into(),
        backup_on_shutdown: Some(true),
        backup_keep: 1,
    };
    validate_storage(&cfg).unwrap();
    assert!(dir.path().join("autorouter.db").exists());
}

#[test]
fn rotate_backup_creates_copy() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("autorouter.db");
    fs::write(&db, b"data").unwrap();
    rotate_backup(&db, 1).unwrap();
    let backup = db.with_extension("db.bak.1");
    assert!(backup.exists());
    assert_eq!(fs::read(&backup).unwrap(), b"data");
}

#[test]
fn rotate_backup_disabled_when_keep_zero() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("autorouter.db");
    fs::write(&db, b"data").unwrap();
    rotate_backup(&db, 0).unwrap();
    let backup = db.with_extension("db.bak.1");
    assert!(!backup.exists());
}

#[test]
fn rotate_backup_rolls_through_keep() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("autorouter.db");
    fs::write(&db, b"v1").unwrap();
    rotate_backup(&db, 3).unwrap();
    fs::write(&db, b"v2").unwrap();
    rotate_backup(&db, 3).unwrap();
    fs::write(&db, b"v3").unwrap();
    rotate_backup(&db, 3).unwrap();
    // After 3 rotations with keep=3, the .bak.1..3 files should exist
    // and the original (newest) should be in .bak.1.
    assert!(db.with_extension("db.bak.1").exists());
    assert!(db.with_extension("db.bak.2").exists());
    assert!(db.with_extension("db.bak.3").exists());
    assert_eq!(fs::read(db.with_extension("db.bak.1")).unwrap(), b"v3");
    // Now trigger a 4th rotation; the oldest should be removed.
    fs::write(&db, b"v4").unwrap();
    rotate_backup(&db, 3).unwrap();
    assert!(!db.with_extension("db.bak.4").exists());
    assert!(db.with_extension("db.bak.1").exists());
}
