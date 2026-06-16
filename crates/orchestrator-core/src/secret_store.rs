//! Project-scoped secrets backed by the OS keychain.
//!
//! v0.5.8 ships a small core: an OS-keychain backend (macOS Keychain,
//! libsecret on Linux, Windows Credential Manager) plus a tiny on-disk
//! index that tracks which KEYs belong to the current `repo_scope`.
//! Listing keychain items by service prefix is platform-specific; the
//! index file is the portable shortcut.
//!
//! Values themselves never touch disk — they live in the keychain.
//! The index stores key names only and is therefore safe to back up
//! alongside the rest of `~/.animus/<repo-scope>/`.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Service-name prefix written into every keychain entry. The
/// `repo_scope` is appended so two projects with the same KEY do not
/// collide in the user's keychain.
pub const KEYCHAIN_SERVICE_PREFIX: &str = "animus:";

/// File name for the per-scope index that lists known KEYs. Lives at
/// `~/.animus/<repo-scope>/secrets/index.json`.
pub const INDEX_FILE_NAME: &str = "index.json";

/// Subdirectory inside the scoped state root.
pub const SECRETS_DIR_NAME: &str = "secrets";

/// Upper bound (bytes) on the cumulative size of injected secret values
/// merged into a plugin's child environment. The spawn-path merger
/// refuses to inject anything past this limit; see
/// `daemon/process-spawn` design notes for the rationale.
pub const MAX_INJECTED_ENV_BYTES: usize = 1024 * 1024;

/// Errors surfaced by the [`SecretStore`] interface.
#[derive(Debug, Error)]
pub enum SecretStoreError {
    #[error("secret KEY {0:?} is empty")]
    EmptyKey(String),
    #[error("secret KEY {0:?} is not valid: must start with a letter or underscore and contain only A-Z, 0-9, and underscore")]
    InvalidKey(String),
    #[error("secret KEY {0:?} is not present in this project's keychain index")]
    NotFound(String),
    #[error("keychain backend reported: {0}")]
    Backend(String),
    #[error("secrets index io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("secrets index parse error at {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

/// Result alias for the secret-store surface.
pub type SecretStoreResult<T> = Result<T, SecretStoreError>;

/// Trait describing the small surface the rest of the codebase needs.
///
/// Two impls exist:
///
/// - [`KeyringSecretStore`] — production, talks to the OS keychain.
/// - [`MockSecretStore`] — in-memory map for tests + CI without a
///   D-Bus session.
pub trait SecretStore: Send + Sync {
    /// Store `value` under `key` for this scope. Overwrites any prior
    /// value silently.
    fn set(&self, key: &str, value: &str) -> SecretStoreResult<()>;

    /// Fetch the value stored under `key`, or `Ok(None)` if the key is
    /// not present.
    fn get(&self, key: &str) -> SecretStoreResult<Option<String>>;

    /// Remove the value stored under `key`. Returns `Ok(false)` when
    /// the key was not present.
    fn delete(&self, key: &str) -> SecretStoreResult<bool>;

    /// Return every KEY tracked for this scope. Values are never
    /// returned by this method — use [`SecretStore::snapshot_for_spawn`]
    /// when you need the whole map (e.g. plugin spawn merge).
    fn list_keys(&self) -> SecretStoreResult<Vec<String>>;

    /// Read every (KEY, VALUE) pair tracked for this scope. Used by the
    /// plugin-host spawn path to merge values into the child env.
    /// Best-effort per key: any per-key backend error degrades to
    /// "skip that key" so a single missing keychain item never blocks
    /// the spawn.
    fn snapshot_for_spawn(&self) -> SecretStoreResult<BTreeMap<String, String>> {
        let mut out = BTreeMap::new();
        for key in self.list_keys()? {
            match self.get(&key) {
                Ok(Some(value)) => {
                    out.insert(key, value);
                }
                Ok(None) | Err(_) => continue,
            }
        }
        Ok(out)
    }
}

/// A boxed store is itself a store, so [`build_secret_store`]'s
/// `Box<dyn SecretStore>` can be passed anywhere `S: SecretStore` is expected
/// (e.g. the daemon's generic snapshot/resolver adapters).
impl SecretStore for Box<dyn SecretStore> {
    fn set(&self, key: &str, value: &str) -> SecretStoreResult<()> {
        (**self).set(key, value)
    }
    fn get(&self, key: &str) -> SecretStoreResult<Option<String>> {
        (**self).get(key)
    }
    fn delete(&self, key: &str) -> SecretStoreResult<bool> {
        (**self).delete(key)
    }
    fn list_keys(&self) -> SecretStoreResult<Vec<String>> {
        (**self).list_keys()
    }
    fn snapshot_for_spawn(&self) -> SecretStoreResult<BTreeMap<String, String>> {
        (**self).snapshot_for_spawn()
    }
}

/// Validate the secret KEY shape. Mirrors the workflow-YAML env var
/// rule so secrets and interpolated env vars share the same alphabet.
pub fn validate_key(key: &str) -> SecretStoreResult<()> {
    if key.is_empty() {
        return Err(SecretStoreError::EmptyKey(key.to_string()));
    }
    let mut chars = key.chars();
    let first = chars.next().expect("non-empty checked above");
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return Err(SecretStoreError::InvalidKey(key.to_string()));
    }
    for ch in chars {
        if !(ch == '_' || ch.is_ascii_alphanumeric()) {
            return Err(SecretStoreError::InvalidKey(key.to_string()));
        }
    }
    Ok(())
}

/// On-disk layout for the per-scope key index.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct IndexFile {
    /// Schema version. Bump if the file shape changes.
    version: u32,
    /// Known KEY names. Sorted on write so the file diff-friendly.
    keys: Vec<String>,
}

impl IndexFile {
    const CURRENT_VERSION: u32 = 1;
}

/// Read the index file for `scoped_root`. Missing file returns an empty
/// index; that matches "no secrets stored yet".
fn read_index(scoped_root: &Path) -> SecretStoreResult<IndexFile> {
    let path = index_path(scoped_root);
    match fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).map_err(|err| SecretStoreError::Parse { path, source: err }),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(IndexFile::default()),
        Err(err) => Err(SecretStoreError::Io { path, source: err }),
    }
}

fn write_index(scoped_root: &Path, mut file: IndexFile) -> SecretStoreResult<()> {
    file.version = IndexFile::CURRENT_VERSION;
    file.keys.sort();
    file.keys.dedup();
    let dir = scoped_root.join(SECRETS_DIR_NAME);
    fs::create_dir_all(&dir).map_err(|err| SecretStoreError::Io { path: dir.clone(), source: err })?;
    let path = index_path(scoped_root);
    let body = serde_json::to_string_pretty(&file)
        .map_err(|err| SecretStoreError::Parse { path: path.clone(), source: err })?;
    // Atomic replace: write to a sibling tempfile and rename over the
    // target. A concurrent reader (or a crash mid-write) will see
    // either the prior intact file or the new intact file, never a
    // truncated half-written index. The advisory lock taken by the
    // mutation paths still serializes writers; this rename hardens the
    // reader path against unlocked reads. (codex round-9 P2.)
    let tmp_path = dir.join(format!("{}.tmp", INDEX_FILE_NAME));
    fs::write(&tmp_path, body).map_err(|err| SecretStoreError::Io { path: tmp_path.clone(), source: err })?;
    // On Windows, `fs::rename` refuses to overwrite an existing file —
    // use a fallback that copies + removes the temp on platforms where
    // the OS lacks a replace-atomic rename. (codex round-10 P2.)
    rename_replace(&tmp_path, &path)?;
    Ok(())
}

fn rename_replace(src: &Path, dst: &Path) -> SecretStoreResult<()> {
    #[cfg(windows)]
    {
        // `fs::rename` on Windows fails when `dst` exists. Try the
        // direct rename first (covers first-write); on AlreadyExists
        // fall back to remove+rename, which is the standard CRT
        // pattern. This keeps a small race window where a concurrent
        // reader could see `dst` missing — the advisory lock taken by
        // the writer prevents concurrent writers, and unlocked readers
        // tolerate `NotFound` as "no secrets yet".
        match fs::rename(src, dst) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                let _ = fs::remove_file(dst);
                fs::rename(src, dst).map_err(|err| SecretStoreError::Io { path: dst.to_path_buf(), source: err })
            }
            Err(err) => Err(SecretStoreError::Io { path: dst.to_path_buf(), source: err }),
        }
    }
    #[cfg(not(windows))]
    {
        fs::rename(src, dst).map_err(|err| SecretStoreError::Io { path: dst.to_path_buf(), source: err })
    }
}

/// Absolute path to the per-scope index file.
#[must_use]
pub fn index_path(scoped_root: &Path) -> PathBuf {
    scoped_root.join(SECRETS_DIR_NAME).join(INDEX_FILE_NAME)
}

/// Path to the advisory lock file that serializes concurrent
/// `read_index → mutate → write_index` operations. Lives next to the
/// index so the lock and the data share a parent directory; the lock
/// file itself never holds secret data. (codex round-8 P2.)
fn index_lock_path(scoped_root: &Path) -> PathBuf {
    scoped_root.join(SECRETS_DIR_NAME).join("index.lock")
}

/// Acquire an exclusive advisory lock on the per-scope index. The
/// returned file MUST be held for the entire read-modify-write of the
/// index; dropping it releases the lock.
fn lock_index(scoped_root: &Path) -> SecretStoreResult<std::fs::File> {
    let dir = scoped_root.join(SECRETS_DIR_NAME);
    fs::create_dir_all(&dir).map_err(|err| SecretStoreError::Io { path: dir.clone(), source: err })?;
    let path = index_lock_path(scoped_root);
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|err| SecretStoreError::Io { path: path.clone(), source: err })?;
    file.lock_exclusive().map_err(|err| SecretStoreError::Io { path, source: err })?;
    Ok(file)
}

/// Compose the keychain service-name string for a given `repo_scope`.
/// Centralized so the CLI and plugin-host agree on the namespace.
#[must_use]
pub fn keychain_service_name(repo_scope: &str) -> String {
    format!("{KEYCHAIN_SERVICE_PREFIX}{repo_scope}")
}

/// Production [`SecretStore`] backed by the keyring crate.
///
/// `service` is the keychain service-name field; build via
/// [`keychain_service_name`]. `scoped_root` is the per-project scoped
/// state directory (typically `~/.animus/<repo-scope>/`). The index
/// file under `scoped_root/secrets/index.json` tracks which KEYs are
/// stored for this scope.
pub struct KeyringSecretStore {
    service: String,
    scoped_root: PathBuf,
}

impl KeyringSecretStore {
    /// Build a store bound to `repo_scope` and `scoped_root`.
    #[must_use]
    pub fn new(repo_scope: &str, scoped_root: impl Into<PathBuf>) -> Self {
        Self { service: keychain_service_name(repo_scope), scoped_root: scoped_root.into() }
    }

    /// Resolved keychain service name.
    #[must_use]
    pub fn service(&self) -> &str {
        &self.service
    }

    fn entry(&self, key: &str) -> SecretStoreResult<keyring::Entry> {
        keyring::Entry::new(&self.service, key).map_err(|err| SecretStoreError::Backend(err.to_string()))
    }
}

impl SecretStore for KeyringSecretStore {
    fn set(&self, key: &str, value: &str) -> SecretStoreResult<()> {
        validate_key(key)?;
        // Serialize concurrent `secret set` / `secret rm` /
        // `import-env` runs through an advisory file lock so two
        // processes can't race the read-modify-write of `index.json`.
        // The keychain write happens inside the lock too so a crash
        // after the keychain write but before the index update can't
        // leave behind an indexed key with no value. (codex round-8 P2.)
        let _lock = lock_index(&self.scoped_root)?;
        let entry = self.entry(key)?;
        entry.set_password(value).map_err(|err| SecretStoreError::Backend(err.to_string()))?;
        let mut index = read_index(&self.scoped_root)?;
        if !index.keys.iter().any(|k| k == key) {
            index.keys.push(key.to_string());
        }
        write_index(&self.scoped_root, index)
    }

    fn get(&self, key: &str) -> SecretStoreResult<Option<String>> {
        validate_key(key)?;
        // Treat the per-scope index as authoritative: a key not in
        // `index.json` is reported as absent even if a stale keychain
        // entry survives. Matches `snapshot_filtered` and
        // `WorkflowSecretResolver::resolve` semantics. (codex round-11 P2.)
        let index = read_index(&self.scoped_root)?;
        if !index.keys.iter().any(|k| k == key) {
            return Ok(None);
        }
        let entry = self.entry(key)?;
        match entry.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(SecretStoreError::Backend(err.to_string())),
        }
    }

    fn delete(&self, key: &str) -> SecretStoreResult<bool> {
        validate_key(key)?;
        let _lock = lock_index(&self.scoped_root)?;
        let entry = self.entry(key)?;
        let removed = match entry.delete_credential() {
            Ok(()) => true,
            Err(keyring::Error::NoEntry) => false,
            Err(err) => return Err(SecretStoreError::Backend(err.to_string())),
        };
        let mut index = read_index(&self.scoped_root)?;
        let before = index.keys.len();
        index.keys.retain(|k| k != key);
        let index_changed = index.keys.len() != before;
        if index_changed {
            write_index(&self.scoped_root, index)?;
        }
        Ok(removed || index_changed)
    }

    fn list_keys(&self) -> SecretStoreResult<Vec<String>> {
        let mut index = read_index(&self.scoped_root)?;
        index.keys.sort();
        index.keys.dedup();
        Ok(index.keys)
    }
}

/// In-memory [`SecretStore`] used by tests and by builds that can't or
/// won't reach a real keychain backend.
#[derive(Debug, Default)]
pub struct MockSecretStore {
    inner: std::sync::Mutex<BTreeMap<String, String>>,
}

impl MockSecretStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed the store with `(key, value)` pairs.
    #[must_use]
    pub fn with_entries<I, K, V>(entries: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let store = Self::new();
        for (k, v) in entries {
            store.inner.lock().expect("mock secret store mutex poisoned").insert(k.into(), v.into());
        }
        store
    }
}

impl SecretStore for MockSecretStore {
    fn set(&self, key: &str, value: &str) -> SecretStoreResult<()> {
        validate_key(key)?;
        self.inner.lock().expect("mock secret store mutex poisoned").insert(key.to_string(), value.to_string());
        Ok(())
    }

    fn get(&self, key: &str) -> SecretStoreResult<Option<String>> {
        validate_key(key)?;
        Ok(self.inner.lock().expect("mock secret store mutex poisoned").get(key).cloned())
    }

    fn delete(&self, key: &str) -> SecretStoreResult<bool> {
        validate_key(key)?;
        Ok(self.inner.lock().expect("mock secret store mutex poisoned").remove(key).is_some())
    }

    fn list_keys(&self) -> SecretStoreResult<Vec<String>> {
        let guard = self.inner.lock().expect("mock secret store mutex poisoned");
        let mut keys: Vec<String> = guard.keys().cloned().collect();
        keys.sort();
        Ok(keys)
    }

    fn snapshot_for_spawn(&self) -> SecretStoreResult<BTreeMap<String, String>> {
        Ok(self.inner.lock().expect("mock secret store mutex poisoned").clone())
    }
}

/// Cap an env merge map at [`MAX_INJECTED_ENV_BYTES`] cumulative bytes.
///
/// Cumulative size is computed as the sum of `key.len() + value.len()`
/// for every entry. When the cap is exceeded, returns `Err(actual)` so
/// the caller can warn instead of silently truncating.
pub fn enforce_injection_cap(entries: &BTreeMap<String, String>) -> Result<(), usize> {
    let mut total: usize = 0;
    for (k, v) in entries {
        total = total.saturating_add(k.len()).saturating_add(v.len());
        if total > MAX_INJECTED_ENV_BYTES {
            return Err(total);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_key_accepts_standard_env_shape() {
        validate_key("LINEAR_API_TOKEN").unwrap();
        validate_key("_X").unwrap();
        validate_key("A1").unwrap();
    }

    #[test]
    fn validate_key_rejects_bad_shapes() {
        assert!(matches!(validate_key(""), Err(SecretStoreError::EmptyKey(_))));
        assert!(matches!(validate_key("1A"), Err(SecretStoreError::InvalidKey(_))));
        assert!(matches!(validate_key("A-B"), Err(SecretStoreError::InvalidKey(_))));
        assert!(matches!(validate_key("A.B"), Err(SecretStoreError::InvalidKey(_))));
    }

    #[test]
    fn mock_store_round_trip() {
        let store = MockSecretStore::new();
        assert_eq!(store.list_keys().unwrap(), Vec::<String>::new());
        store.set("FOO", "bar").unwrap();
        store.set("BAZ", "qux").unwrap();
        let mut keys = store.list_keys().unwrap();
        keys.sort();
        assert_eq!(keys, vec!["BAZ".to_string(), "FOO".to_string()]);
        assert_eq!(store.get("FOO").unwrap().as_deref(), Some("bar"));
        assert_eq!(store.get("MISSING").unwrap(), None);
        assert!(store.delete("FOO").unwrap());
        assert!(!store.delete("FOO").unwrap());
        assert_eq!(store.list_keys().unwrap(), vec!["BAZ".to_string()]);
    }

    #[test]
    fn snapshot_returns_all_pairs() {
        let store = MockSecretStore::with_entries([("A", "1"), ("B", "2")]);
        let snap = store.snapshot_for_spawn().unwrap();
        assert_eq!(snap.len(), 2);
        assert_eq!(snap.get("A").map(String::as_str), Some("1"));
        assert_eq!(snap.get("B").map(String::as_str), Some("2"));
    }

    #[test]
    fn enforce_cap_passes_under_limit() {
        let mut map = BTreeMap::new();
        map.insert("K".to_string(), "v".to_string());
        enforce_injection_cap(&map).unwrap();
    }

    #[test]
    fn enforce_cap_fails_over_limit() {
        let mut map = BTreeMap::new();
        let big = "x".repeat(MAX_INJECTED_ENV_BYTES + 1);
        map.insert("BIG".to_string(), big);
        let err = enforce_injection_cap(&map).unwrap_err();
        assert!(err > MAX_INJECTED_ENV_BYTES);
    }

    #[test]
    fn keyring_store_index_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KeyringSecretStore::new("test-scope-secrets-roundtrip", tmp.path());
        let _ = store.delete("ANIMUS_TEST_KEY1");
        let _ = store.delete("ANIMUS_TEST_KEY2");
        if store.set("ANIMUS_TEST_KEY1", "val-1").is_err() {
            // No keychain backend available in this environment; skip.
            return;
        }
        store.set("ANIMUS_TEST_KEY2", "val-2").unwrap();
        let keys = store.list_keys().unwrap();
        assert!(keys.contains(&"ANIMUS_TEST_KEY1".to_string()));
        assert!(keys.contains(&"ANIMUS_TEST_KEY2".to_string()));
        assert_eq!(store.get("ANIMUS_TEST_KEY1").unwrap().as_deref(), Some("val-1"));
        assert!(store.delete("ANIMUS_TEST_KEY1").unwrap());
        let keys = store.list_keys().unwrap();
        assert!(!keys.contains(&"ANIMUS_TEST_KEY1".to_string()));
        let _ = store.delete("ANIMUS_TEST_KEY2");
    }

    #[test]
    fn keychain_service_name_includes_scope() {
        let svc = keychain_service_name("auth-main-5ba84d1bbafc");
        assert_eq!(svc, "animus:auth-main-5ba84d1bbafc");
    }
}
