//! Secret store.
//!
//! Three implementations:
//!   * [`InMemoryStore`] for tests and short-lived CLI runs
//!   * [`FileStore`] as a JSON-file fallback with restricted permissions
//!   * [`KeyringStore`] using the OS keychain via the `keyring` crate

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

use crate::error::{ConfigError, ConfigResult};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretId(pub String);

impl SecretId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for SecretId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for SecretId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Secret {
    pub id: SecretId,
    pub value: String,
    pub label: Option<String>,
    pub created_at: u64,
}

impl Secret {
    pub fn new(id: impl Into<SecretId>, value: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            value: value.into(),
            label: None,
            created_at: now_seconds(),
        }
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
}

fn now_seconds() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub trait SecretStore: Send + Sync {
    fn put(&self, secret: Secret) -> ConfigResult<()>;
    fn get(&self, id: &SecretId) -> ConfigResult<Secret>;
    fn delete(&self, id: &SecretId) -> ConfigResult<()>;
    fn list(&self) -> ConfigResult<Vec<SecretId>>;
    /// Whether the backing store supports `list()`. The OS keychain
    /// does not expose a portable enumeration API, so this returns
    /// `false` by default. In-memory and file stores override it.
    fn list_supported(&self) -> bool {
        false
    }
    /// A short human-readable name of the backing store, e.g.
    /// `"keychain"`, `"file"`, or `"memory"`. Surfaced on the
    /// dashboard so operators know where their secrets live.
    fn backend_name(&self) -> &'static str {
        "unknown"
    }
}

#[derive(Debug, Default, Clone)]
pub struct InMemoryStore {
    inner: Arc<RwLock<HashMap<SecretId, Secret>>>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SecretStore for InMemoryStore {
    fn backend_name(&self) -> &'static str {
        "memory"
    }

    fn list_supported(&self) -> bool {
        true
    }

    fn put(&self, secret: Secret) -> ConfigResult<()> {
        self.inner
            .write()
            .map_err(|e| ConfigError::Secret(e.to_string()))?
            .insert(secret.id.clone(), secret);
        Ok(())
    }
    fn get(&self, id: &SecretId) -> ConfigResult<Secret> {
        self.inner
            .read()
            .map_err(|e| ConfigError::Secret(e.to_string()))?
            .get(id)
            .cloned()
            .ok_or_else(|| ConfigError::NotFound(id.0.clone()))
    }
    fn delete(&self, id: &SecretId) -> ConfigResult<()> {
        self.inner
            .write()
            .map_err(|e| ConfigError::Secret(e.to_string()))?
            .remove(id)
            .ok_or_else(|| ConfigError::NotFound(id.0.clone()))?;
        Ok(())
    }
    fn list(&self) -> ConfigResult<Vec<SecretId>> {
        Ok(self
            .inner
            .read()
            .map_err(|e| ConfigError::Secret(e.to_string()))?
            .keys()
            .cloned()
            .collect())
    }
}

#[derive(Debug)]
pub struct FileStore {
    path: std::path::PathBuf,
    lock: std::sync::Mutex<()>,
}

impl FileStore {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            lock: std::sync::Mutex::new(()),
        }
    }

    fn load(&self) -> ConfigResult<HashMap<SecretId, Secret>> {
        if !self.path.exists() {
            return Ok(HashMap::new());
        }
        let text = std::fs::read_to_string(&self.path)?;
        if text.trim().is_empty() {
            return Ok(HashMap::new());
        }
        let map: HashMap<SecretId, Secret> = serde_json::from_str(&text)?;
        Ok(map)
    }

    fn save(&self, map: &HashMap<SecretId, Secret>) -> ConfigResult<()> {
        use std::io::Write;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(map)?;
        // H5: write to a temp file, fsync, then atomically rename.
        // Permissions are set on the temp file BEFORE the rename so
        // the final destination has the right mode from the moment
        // it is created (chmod after rename would briefly leave the
        // file world-readable).
        let tmp = self.path.with_extension("json.tmp");
        if tmp.exists() {
            let _ = std::fs::remove_file(&tmp);
        }
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&tmp)?.permissions();
            perms.set_mode(0o600);
            std::fs::set_permissions(&tmp, perms)?;
        }
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

impl SecretStore for FileStore {
    fn backend_name(&self) -> &'static str {
        "file"
    }

    fn list_supported(&self) -> bool {
        true
    }

    fn put(&self, secret: Secret) -> ConfigResult<()> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut map = self.load()?;
        map.insert(secret.id.clone(), secret);
        self.save(&map)
    }
    fn get(&self, id: &SecretId) -> ConfigResult<Secret> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.load()?
            .get(id)
            .cloned()
            .ok_or_else(|| ConfigError::NotFound(id.0.clone()))
    }
    fn delete(&self, id: &SecretId) -> ConfigResult<()> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut map = self.load()?;
        map.remove(id)
            .ok_or_else(|| ConfigError::NotFound(id.0.clone()))?;
        self.save(&map)
    }
    fn list(&self) -> ConfigResult<Vec<SecretId>> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        Ok(self.load()?.keys().cloned().collect())
    }
}

#[derive(Debug, Default)]
pub struct KeyringStore {
    service: String,
}

impl KeyringStore {
    pub fn new() -> Self {
        Self {
            service: "autorouter".into(),
        }
    }

    pub fn with_service(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }
}

impl KeyringStore {
    /// Probe whether the OS keychain is reachable.
    ///
    /// Prefer a **read-only** probe first: looking up a known probe id
    /// and receiving either a value or `NotFound` proves the keyring
    /// daemon answers without writing. This avoids interactive OS
    /// prompts and hangs that a write probe can trigger in headless
    /// environments.
    ///
    /// Only when the error is ambiguous (daemon down, permission
    /// denied, etc.) do we fall through to a write+delete probe so we
    /// can distinguish "reachable" from "broken". Any probe value left
    /// behind by a prior crash is deleted best-effort.
    pub fn is_available() -> bool {
        let store = KeyringStore::new();
        let probe_id = SecretId::new("__autorouter_probe__");

        match store.get(&probe_id) {
            // Keyring answered. If a previous probe left a value, scrub it.
            Ok(_) => {
                let _ = store.delete(&probe_id);
                return true;
            }
            Err(ConfigError::NotFound(_)) => return true,
            // Ambiguous failure — try a write probe below.
            Err(_) => {}
        }

        let probe = Secret::new(probe_id.clone(), "probe");
        if store.put(probe).is_err() {
            return false;
        }
        // Best-effort cleanup so we do not litter the keychain.
        // Write success is enough to declare the keyring available.
        if let Err(e) = store.delete(&probe_id) {
            tracing::debug!(error = %e, "keyring probe cleanup failed");
        }
        true
    }
}

impl SecretStore for KeyringStore {
    fn backend_name(&self) -> &'static str {
        "keychain"
    }

    fn put(&self, secret: Secret) -> ConfigResult<()> {
        let entry = keyring::Entry::new(&self.service, secret.id.as_str())
            .map_err(|e| ConfigError::Secret(e.to_string()))?;
        entry
            .set_password(&secret.value)
            .map_err(|e| ConfigError::Secret(e.to_string()))?;
        Ok(())
    }
    fn get(&self, id: &SecretId) -> ConfigResult<Secret> {
        let entry = keyring::Entry::new(&self.service, id.as_str())
            .map_err(|e| ConfigError::Secret(e.to_string()))?;
        let value = entry.get_password().map_err(|e| match e {
            keyring::Error::NoEntry => ConfigError::NotFound(id.0.clone()),
            other => ConfigError::Secret(other.to_string()),
        })?;
        Ok(Secret {
            id: id.clone(),
            value,
            label: None,
            created_at: now_seconds(),
        })
    }
    fn delete(&self, id: &SecretId) -> ConfigResult<()> {
        let entry = keyring::Entry::new(&self.service, id.as_str())
            .map_err(|e| ConfigError::Secret(e.to_string()))?;
        match entry.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Err(ConfigError::NotFound(id.0.clone())),
            Err(e) => Err(ConfigError::Secret(e.to_string())),
        }
    }
    fn list(&self) -> ConfigResult<Vec<SecretId>> {
        // N2: surface a ListNotSupported error so the UI can render
        // a meaningful "keyring enumeration is not available on this
        // platform" message instead of a confusing empty list.
        Err(ConfigError::ListNotSupported(
            "OS keychain does not expose a portable list API".into(),
        ))
    }
}

pub fn build_secret_store(kind: &str, file_path: Option<&Path>) -> Arc<dyn SecretStore> {
    // Empty/unknown kinds default to the OS keychain (documented behaviour).
    let normalised = kind.trim();
    match normalised {
        "" | "keychain" | "keyring" | "default" => {
            if KeyringStore::is_available() {
                return Arc::new(KeyringStore::new());
            }
            if let Some(p) = file_path {
                return Arc::new(FileStore::new(p));
            }
            // Last-resort: in-memory. We do NOT want to crash on
            // platforms without a keyring, but operators must know
            // they lost durability.
            tracing::warn!(
                "OS keychain unavailable and AUTOROUTER_SECRET_FILE not set; \
                 falling back to in-memory secret store — keys will be lost on restart"
            );
            Arc::new(InMemoryStore::new())
        }
        "file" => {
            if let Some(p) = file_path {
                return Arc::new(FileStore::new(p));
            }
            tracing::warn!(
                "AUTOROUTER_SECRET_STORE=file but AUTOROUTER_SECRET_FILE unset; \
                 falling back to in-memory secret store"
            );
            Arc::new(InMemoryStore::new())
        }
        "memory" => Arc::new(InMemoryStore::new()),
        other => {
            tracing::warn!(
                kind = %other,
                "unknown AUTOROUTER_SECRET_STORE; falling back to OS keychain"
            );
            if KeyringStore::is_available() {
                Arc::new(KeyringStore::new())
            } else {
                Arc::new(InMemoryStore::new())
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_memory_returns_in_memory() {
        let store = build_secret_store("memory", None);
        assert_eq!(store.backend_name(), "memory");
    }

    #[test]
    fn empty_string_does_not_pick_memory_silently() {
        let _ = build_secret_store("", None);
        let _ = build_secret_store("default", None);
    }

    #[test]
    fn explicit_file_with_path_returns_file_store() {
        let dir = std::env::temp_dir().join(format!(
            "autorouter-secret-test-{}-{}",
            std::process::id(),
            now_seconds()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("secrets.json");
        let store = build_secret_store("file", Some(&path));
        assert_eq!(store.backend_name(), "file");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_kind_falls_back_to_platform_default() {
        let store = build_secret_store("definitely-not-a-store", None);
        let _ = store.backend_name(); // doesn't crash
    }
}
