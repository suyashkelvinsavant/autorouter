//! Tests for the secret store.

use autorouter_config::{FileStore, InMemoryStore, Secret, SecretId, SecretStore};
use tempfile::tempdir;

#[test]
fn in_memory_round_trip() {
    let store = InMemoryStore::new();
    store.put(Secret::new("openai", "sk-test")).unwrap();
    let s = store.get(&SecretId::new("openai")).unwrap();
    assert_eq!(s.value, "sk-test");
    assert!(store.list().unwrap().contains(&SecretId::new("openai")));
}

#[test]
fn in_memory_not_found() {
    let store = InMemoryStore::new();
    let err = store.get(&SecretId::new("nope")).unwrap_err();
    assert!(err.to_string().contains("not found"));
}

#[test]
fn in_memory_delete() {
    let store = InMemoryStore::new();
    store.put(Secret::new("k", "v")).unwrap();
    store.delete(&SecretId::new("k")).unwrap();
    assert!(store.get(&SecretId::new("k")).is_err());
}

#[test]
fn file_store_persists_across_instances() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("secrets.json");
    {
        let s = FileStore::new(&path);
        s.put(Secret::new("openai", "sk-1")).unwrap();
        s.put(Secret::new("anthropic", "sk-ant-1")).unwrap();
    }
    let s2 = FileStore::new(&path);
    assert_eq!(s2.get(&SecretId::new("openai")).unwrap().value, "sk-1");
    assert_eq!(
        s2.get(&SecretId::new("anthropic")).unwrap().value,
        "sk-ant-1"
    );
    let mut ids = s2.list().unwrap();
    ids.sort();
    assert_eq!(
        ids,
        vec![SecretId::new("anthropic"), SecretId::new("openai")]
    );
}

#[test]
fn file_store_overwrites() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("secrets.json");
    let s = FileStore::new(&path);
    s.put(Secret::new("k", "v1")).unwrap();
    s.put(Secret::new("k", "v2")).unwrap();
    assert_eq!(s.get(&SecretId::new("k")).unwrap().value, "v2");
}

#[test]
fn file_store_restricted_permissions_on_unix() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        let s = FileStore::new(&path);
        s.put(Secret::new("k", "v")).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0600, got {mode:o}");
    }
}
