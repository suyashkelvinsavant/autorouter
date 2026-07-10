//! Tests for the recovery helpers.

use std::fs;

use autorouter_config::StorageConfig;
use autorouter_observability::{prune_timestamped_backups, rotate_backup, validate_storage};
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
fn rotate_backup_keep_zero_removes_every_existing_sibling_backup() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("autorouter.db");
    fs::write(&db, b"data").unwrap();
    for n in [1, 10, 99] {
        fs::write(dir.path().join(format!("autorouter.db.bak.{n}")), b"old").unwrap();
    }

    rotate_backup(&db, 0).unwrap();

    assert!(!dir.path().join("autorouter.db.bak.1").exists());
    assert!(!dir.path().join("autorouter.db.bak.10").exists());
    assert!(!dir.path().join("autorouter.db.bak.99").exists());
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

#[test]
fn prune_timestamped_keeps_newest_n() {
    let dir = tempdir().unwrap();
    let backup_dir = dir.path().join("backups");
    fs::create_dir_all(&backup_dir).unwrap();

    // Stamps sort lexicographically (UTC `%Y%m%dT%H%M%SZ` layout).
    let names = [
        "autorouter.db.20260101T000000Z",
        "autorouter.db.20260102T000000Z",
        "autorouter.db.20260103T000000Z",
        "autorouter.db.20260104T000000Z",
    ];
    for name in &names {
        fs::write(backup_dir.join(name), name.as_bytes()).unwrap();
    }
    // Unrelated file must not be touched.
    fs::write(backup_dir.join("notes.txt"), b"keep me").unwrap();
    // Bare db name (no stamp) must not match the prefix rule.
    fs::write(backup_dir.join("autorouter.db"), b"live-copy").unwrap();

    let deleted = prune_timestamped_backups(&backup_dir, "autorouter.db", 2).unwrap();
    assert_eq!(deleted, 2);

    // Newest two stamps remain; oldest two gone.
    assert!(!backup_dir.join(names[0]).exists());
    assert!(!backup_dir.join(names[1]).exists());
    assert!(backup_dir.join(names[2]).exists());
    assert!(backup_dir.join(names[3]).exists());
    assert!(backup_dir.join("notes.txt").exists());
    assert!(backup_dir.join("autorouter.db").exists());
}

#[test]
fn prune_timestamped_keep_zero_removes_all_matching() {
    let dir = tempdir().unwrap();
    let backup_dir = dir.path().join("backups");
    fs::create_dir_all(&backup_dir).unwrap();
    fs::write(backup_dir.join("autorouter.db.20260101T000000Z"), b"a").unwrap();
    fs::write(backup_dir.join("autorouter.db.20260102T000000Z"), b"b").unwrap();

    let deleted = prune_timestamped_backups(&backup_dir, "autorouter.db", 0).unwrap();
    assert_eq!(deleted, 2);
    assert!(backup_dir.read_dir().unwrap().next().is_none());
}

#[test]
fn prune_timestamped_missing_dir_is_ok() {
    let dir = tempdir().unwrap();
    let missing = dir.path().join("nope");
    let deleted = prune_timestamped_backups(&missing, "autorouter.db", 3).unwrap();
    assert_eq!(deleted, 0);
}
