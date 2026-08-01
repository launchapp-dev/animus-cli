//! Device-encrypted [`SecretStore`] backend.
//!
//! Secrets live AEAD-sealed in a single `0600` file per repo-scope. A random
//! 32-byte master key seals the secret map; the master key itself is stored
//! *wrapped* under a [`KeySource`] key (device hardware / operator key /
//! passphrase / device-id). Wrapping the master key lets the device/user key
//! rotate without re-encrypting every secret. See
//! `docs/architecture/secret-backends.md` for the threat model.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use rand::RngCore;
use zeroize::Zeroizing;

use crate::secret_keysource::{random_salt, resolve_key_source, KeySourceConfig, KEY_LEN};
use crate::secret_store::{validate_key, SecretStore, SecretStoreError, SecretStoreResult};

const MAGIC: &[u8; 8] = b"ANIMSEC1";
const FORMAT_VERSION: u8 = 1;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
/// Master key (32) + Poly1305 tag (16) once wrapped.
const WRAPPED_MASTER_LEN: usize = KEY_LEN + 16;

fn backend(msg: impl std::fmt::Display) -> SecretStoreError {
    SecretStoreError::Backend(msg.to_string())
}

fn io_err(path: &Path, source: std::io::Error) -> SecretStoreError {
    SecretStoreError::Io { path: path.to_path_buf(), source }
}

/// AEAD seal under `key` with a fresh random nonce; returns `nonce || ciphertext`.
fn seal(key: &[u8; KEY_LEN], plaintext: &[u8], aad: &[u8]) -> SecretStoreResult<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let mut nonce = [0u8; NONCE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce), Payload { msg: plaintext, aad })
        .map_err(|_| backend("AEAD seal failed"))?;
    let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

/// AEAD open of `nonce || ciphertext`. Fails closed on tamper / wrong key.
fn open(key: &[u8; KEY_LEN], blob: &[u8], aad: &[u8]) -> SecretStoreResult<Vec<u8>> {
    if blob.len() < NONCE_LEN {
        return Err(backend("ciphertext too short"));
    }
    let (nonce, ct) = blob.split_at(NONCE_LEN);
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    cipher
        .decrypt(Nonce::from_slice(nonce), Payload { msg: ct, aad })
        .map_err(|_| backend("AEAD verification failed (wrong device key or tampered store)"))
}

/// The decrypted in-memory state.
struct State {
    salt: Vec<u8>,
    master_key: Zeroizing<[u8; KEY_LEN]>,
    entries: BTreeMap<String, String>,
}

/// A [`SecretStore`] that keeps secrets in an AEAD-sealed, device-key-wrapped
/// file. Stateless across calls: each operation reads, decrypts, mutates, and
/// (for writers) atomically rewrites the file.
pub struct DeviceEncryptedSecretStore {
    secrets_path: PathBuf,
    lock_path: PathBuf,
    key_config: KeySourceConfig,
}

impl DeviceEncryptedSecretStore {
    /// Build the store for `scoped_root` (`~/.animus/<repo-scope>/`).
    pub fn new(scoped_root: impl Into<PathBuf>, key_config: KeySourceConfig) -> Self {
        let dir = scoped_root.into().join("secrets");
        Self { secrets_path: dir.join("secrets.enc.v1"), lock_path: dir.join(".secrets.lock"), key_config }
    }

    pub fn path(&self) -> &Path {
        &self.secrets_path
    }

    /// AAD authenticating the format, key-source id, and salt for both seals.
    fn aad(source_id: &str, salt: &[u8]) -> Vec<u8> {
        let mut aad = Vec::new();
        aad.extend_from_slice(MAGIC);
        aad.push(FORMAT_VERSION);
        aad.extend_from_slice(source_id.as_bytes());
        aad.push(0);
        aad.extend_from_slice(salt);
        aad
    }

    /// Read + decrypt the current state, or an empty initialized state when the
    /// file does not yet exist.
    fn read_state(&self) -> SecretStoreResult<State> {
        let raw = match std::fs::read(&self.secrets_path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return self.fresh_state(),
            Err(e) => return Err(io_err(&self.secrets_path, e)),
        };
        self.decode(&raw)
    }

    /// Initialize a brand-new state: fresh salt + fresh random master key, bound
    /// to whatever key source the config selects.
    fn fresh_state(&self) -> SecretStoreResult<State> {
        let salt = random_salt(SALT_LEN);
        // Resolving the source here surfaces config errors (e.g. user-key with no
        // key) at first use rather than silently writing an unrecoverable file.
        resolve_key_source(&self.key_config, &salt).map_err(backend)?;
        let mut master = Zeroizing::new([0u8; KEY_LEN]);
        rand::rngs::OsRng.fill_bytes(master.as_mut_slice());
        Ok(State { salt, master_key: master, entries: BTreeMap::new() })
    }

    /// Parse + authenticate + decrypt the on-disk bytes.
    fn decode(&self, raw: &[u8]) -> SecretStoreResult<State> {
        let mut cur = Cursor::new(raw);
        let magic = cur.take(MAGIC.len())?;
        if magic != MAGIC {
            return Err(backend("not an Animus secret store (bad magic)"));
        }
        let version = cur.take(1)?[0];
        if version != FORMAT_VERSION {
            return Err(backend(format!("unsupported secret store version {version}")));
        }
        let source_id = String::from_utf8(cur.take_lp()?.to_vec()).map_err(|_| backend("bad key-source id"))?;
        let salt = cur.take_lp()?.to_vec();
        let wrapped_master = cur.take_lp()?.to_vec();
        let sealed_body = cur.rest();

        let aad = Self::aad(&source_id, &salt);
        let source = resolve_key_source(&self.key_config, &salt).map_err(backend)?;
        if source.id() != source_id {
            return Err(backend(format!(
                "secret store was written with key source '{source_id}' but '{}' is configured; \
                 set secret_key_source = {source_id} to read it (or migrate)",
                source.id()
            )));
        }
        let wrap_key = source.key().map_err(backend)?;
        let master_bytes = open(&wrap_key, &wrapped_master, &aad)?;
        if master_bytes.len() != KEY_LEN {
            return Err(backend("unwrapped master key has wrong length"));
        }
        let mut master = Zeroizing::new([0u8; KEY_LEN]);
        master.copy_from_slice(&master_bytes);

        let body = open(&master, sealed_body, &aad)?;
        let entries: BTreeMap<String, String> = if body.is_empty() {
            BTreeMap::new()
        } else {
            serde_json::from_slice(&body)
                .map_err(|e| SecretStoreError::Parse { path: self.secrets_path.clone(), source: e })?
        };
        Ok(State { salt, master_key: master, entries })
    }

    /// Encrypt + atomically write the state (`0600`).
    fn write_state(&self, state: &State) -> SecretStoreResult<()> {
        let source = resolve_key_source(&self.key_config, &state.salt).map_err(backend)?;
        let wrap_key = source.key().map_err(backend)?;
        let aad = Self::aad(source.id(), &state.salt);

        let wrapped_master = seal(&wrap_key, state.master_key.as_slice(), &aad)?;
        let body_plain = serde_json::to_vec(&state.entries)
            .map_err(|e| SecretStoreError::Parse { path: self.secrets_path.clone(), source: e })?;
        let sealed_body = seal(&state.master_key, &body_plain, &aad)?;

        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.push(FORMAT_VERSION);
        write_lp(&mut out, source.id().as_bytes());
        write_lp(&mut out, &state.salt);
        write_lp(&mut out, &wrapped_master);
        out.extend_from_slice(&sealed_body);

        debug_assert_eq!(wrapped_master.len(), NONCE_LEN + WRAPPED_MASTER_LEN);
        self.atomic_write(&out)
    }

    fn atomic_write(&self, bytes: &[u8]) -> SecretStoreResult<()> {
        let dir = self.secrets_path.parent().expect("secrets path has a parent");
        std::fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
        restrict_dir(dir);
        let tmp = dir.join(format!(".secrets.{}.tmp", std::process::id()));
        std::fs::write(&tmp, bytes).map_err(|e| io_err(&tmp, e))?;
        restrict_file(&tmp);
        // POSIX `rename` atomically replaces an existing destination; Windows
        // `rename` errors if the destination exists, so fall back to removing
        // the old file first (the advisory write lock serializes this).
        match std::fs::rename(&tmp, &self.secrets_path) {
            Ok(()) => Ok(()),
            Err(_) if self.secrets_path.exists() => {
                std::fs::remove_file(&self.secrets_path).map_err(|e| io_err(&self.secrets_path, e))?;
                std::fs::rename(&tmp, &self.secrets_path).map_err(|e| io_err(&self.secrets_path, e))
            }
            Err(e) => Err(io_err(&self.secrets_path, e)),
        }
    }

    /// Hold an advisory lock across read-modify-write so concurrent writers do
    /// not lose updates.
    fn with_write_lock<T>(&self, f: impl FnOnce() -> SecretStoreResult<T>) -> SecretStoreResult<T> {
        use fs2::FileExt;
        let dir = self.lock_path.parent().expect("lock path has a parent");
        std::fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&self.lock_path)
            .map_err(|e| io_err(&self.lock_path, e))?;
        lock.lock_exclusive().map_err(|e| io_err(&self.lock_path, e))?;
        let r = f();
        let _ = FileExt::unlock(&lock);
        r
    }
}

impl SecretStore for DeviceEncryptedSecretStore {
    fn set(&self, key: &str, value: &str) -> SecretStoreResult<()> {
        validate_key(key)?;
        self.with_write_lock(|| {
            let mut state = self.read_state()?;
            state.entries.insert(key.to_string(), value.to_string());
            self.write_state(&state)
        })
    }

    fn get(&self, key: &str) -> SecretStoreResult<Option<String>> {
        validate_key(key)?;
        Ok(self.read_state()?.entries.get(key).cloned())
    }

    fn delete(&self, key: &str) -> SecretStoreResult<bool> {
        validate_key(key)?;
        self.with_write_lock(|| {
            let mut state = self.read_state()?;
            let removed = state.entries.remove(key).is_some();
            if removed {
                self.write_state(&state)?;
            }
            Ok(removed)
        })
    }

    fn list_keys(&self) -> SecretStoreResult<Vec<String>> {
        Ok(self.read_state()?.entries.keys().cloned().collect())
    }

    fn snapshot_for_spawn(&self) -> SecretStoreResult<BTreeMap<String, String>> {
        // One decrypt for the whole map (cheaper + safer than per-key).
        Ok(self.read_state()?.entries)
    }

    fn backend_label(&self) -> &'static str {
        "device-encrypted store"
    }
}

// --- length-prefixed (u32-LE) framing helpers ---

fn write_lp(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
}

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    fn take(&mut self, n: usize) -> SecretStoreResult<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or_else(|| backend("length overflow"))?;
        if end > self.buf.len() {
            return Err(backend("truncated secret store"));
        }
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    fn take_lp(&mut self) -> SecretStoreResult<&'a [u8]> {
        let len_bytes = self.take(4)?;
        let len = u32::from_le_bytes(len_bytes.try_into().expect("4 bytes")) as usize;
        self.take(len)
    }
    fn rest(&self) -> &'a [u8] {
        &self.buf[self.pos..]
    }
}

#[cfg(unix)]
fn restrict_file(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}
#[cfg(unix)]
fn restrict_dir(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
}
#[cfg(not(unix))]
fn restrict_file(_path: &Path) {}
#[cfg(not(unix))]
fn restrict_dir(_path: &Path) {}

/// Build the configured [`SecretStore`] for a repo scope. Reads the global
/// `secrets` config to choose the backend. Conservative default: the OS keyring
/// (existing installs are unchanged). Uses the device-encrypted store when
/// `backend = device`, when an encrypted store already exists for this scope
/// (a migrated install keeps using it), or when a server key source is
/// configured/injected (headless install — avoids the keyring-unavailable error).
/// This is the single seam the rest of the codebase constructs through.
pub fn build_secret_store(repo_scope: &str, scoped_root: impl Into<PathBuf>) -> Box<dyn SecretStore> {
    let cfg = protocol::Config::load_global_if_exists().and_then(|c| c.secrets).unwrap_or_default();
    build_with_cfg(repo_scope, scoped_root.into(), &cfg)
}

/// Build the configured [`SecretStore`], consulting both the global config
/// (`~/.animus/config.json`) and the project-level `.animus/config.json`. The
/// project config's `secrets` block takes precedence over the global config's,
/// so per-project deployments can override the key source without touching the
/// global config.
///
/// Use this instead of [`build_secret_store`] in surfaces that have access to
/// the project root (e.g. `mcp auth --complete`) so that writing `key_source`
/// into the project-level config is honored end-to-end.
pub fn build_secret_store_for_project(
    repo_scope: &str,
    scoped_root: impl Into<PathBuf>,
    project_root: &Path,
) -> Box<dyn SecretStore> {
    let global = protocol::Config::load_global_if_exists().and_then(|c| c.secrets).unwrap_or_default();
    let project = load_project_secrets_config(project_root);
    let cfg = merge_secrets_config(global, project);
    build_with_cfg(repo_scope, scoped_root.into(), &cfg)
}

/// Build a SPECIFIC backend by name (`"device"` or anything else → keyring),
/// bypassing the auto-selection. Used by `animus secret migrate` to construct
/// both the source and target ends explicitly.
pub fn build_backend(repo_scope: &str, scoped_root: impl Into<PathBuf>, backend: &str) -> Box<dyn SecretStore> {
    let scoped_root = scoped_root.into();
    if backend == "device" {
        let cfg = protocol::Config::load_global_if_exists().and_then(|c| c.secrets).unwrap_or_default();
        Box::new(DeviceEncryptedSecretStore::new(scoped_root, key_source_config(&cfg)))
    } else {
        Box::new(crate::secret_store::KeyringSecretStore::new(repo_scope, scoped_root))
    }
}

/// Same as [`build_backend`] but also loads the project-level `.animus/config.json`
/// so `key_source`/`key_file` set in the project config are honored when explicitly
/// building the device backend (e.g. for `animus secret migrate`). The project
/// config's `secrets` block wins field-by-field over the global config.
pub fn build_backend_for_project(
    repo_scope: &str,
    scoped_root: impl Into<PathBuf>,
    backend: &str,
    project_root: &Path,
) -> Box<dyn SecretStore> {
    let scoped_root = scoped_root.into();
    if backend == "device" {
        let global = protocol::Config::load_global_if_exists().and_then(|c| c.secrets).unwrap_or_default();
        let project = load_project_secrets_config(project_root);
        let cfg = merge_secrets_config(global, project);
        Box::new(DeviceEncryptedSecretStore::new(scoped_root, key_source_config(&cfg)))
    } else {
        Box::new(crate::secret_store::KeyringSecretStore::new(repo_scope, scoped_root))
    }
}

/// Core builder: choose backend from `cfg` and construct the store.
fn build_with_cfg(repo_scope: &str, scoped_root: PathBuf, cfg: &protocol::SecretsConfig) -> Box<dyn SecretStore> {
    let backend = resolve_auto_backend(cfg, &scoped_root);
    if backend == "device" {
        Box::new(DeviceEncryptedSecretStore::new(scoped_root, key_source_config(cfg)))
    } else {
        Box::new(crate::secret_store::KeyringSecretStore::new(repo_scope, scoped_root))
    }
}

/// Resolve which storage backend to use given a [`protocol::SecretsConfig`].
///
/// `auto` rules (applied in order):
/// 1. An operator-configured or env-injected server key source (`user-key` /
///    `passphrase` / `ANIMUS_SECRET_KEY` / `ANIMUS_SECRET_PASSPHRASE`) →
///    `device`. The operator has signaled they want device-encrypted storage;
///    on headless hosts this avoids the OS-keyring-unavailable hard error.
/// 2. A device-encrypted store already exists for this scope → `device`.
///    Post-migration installs continue using the device store.
/// 3. Fall back to `keyring` (existing desktop installs are unchanged).
fn resolve_auto_backend(cfg: &protocol::SecretsConfig, scoped_root: &Path) -> &'static str {
    match cfg.backend.as_deref().unwrap_or("auto") {
        "device" => "device",
        "keyring" | "env" => "keyring",
        _ => {
            if has_server_key_configured(cfg) {
                return "device";
            }
            let device = DeviceEncryptedSecretStore::new(scoped_root.to_path_buf(), key_source_config(cfg));
            if device.path().exists() {
                "device"
            } else {
                "keyring"
            }
        }
    }
}

/// True when a server-appropriate key source is available: explicitly configured
/// via `key_source`, a `key_file` path (honored by `auto` and `user-key`), or
/// injected via the corresponding env var.
fn has_server_key_configured(cfg: &protocol::SecretsConfig) -> bool {
    use crate::secret_keysource::{KeySourceKind, ENV_PASSPHRASE, ENV_USER_KEY};
    let configured_source = cfg.key_source.as_deref().and_then(|source| KeySourceKind::parse(source).ok());
    configured_source.is_some_and(|source| matches!(source, KeySourceKind::UserKey | KeySourceKind::Passphrase))
        || (cfg.key_file.as_deref().is_some_and(|path| !path.trim().is_empty())
            && matches!(configured_source, None | Some(KeySourceKind::Auto | KeySourceKind::UserKey)))
        || std::env::var(ENV_USER_KEY).is_ok_and(|raw| !raw.trim().is_empty())
        || std::env::var(ENV_PASSPHRASE).is_ok_and(|raw| !raw.trim().is_empty())
}

/// Read the project-level `.animus/config.json` and return its `secrets` block.
/// Returns `None` when the file is absent or unparseable (side-effect-free).
fn load_project_secrets_config(project_root: &Path) -> Option<protocol::SecretsConfig> {
    let path = project_root.join(".animus").join("config.json");
    if !path.exists() {
        return None;
    }
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str::<protocol::Config>(&content).ok()?.secrets
}

/// Merge two [`protocol::SecretsConfig`] values; `project` wins field-by-field.
fn merge_secrets_config(
    global: protocol::SecretsConfig,
    project: Option<protocol::SecretsConfig>,
) -> protocol::SecretsConfig {
    let Some(proj) = project else { return global };
    protocol::SecretsConfig {
        backend: proj.backend.or(global.backend),
        key_source: proj.key_source.or(global.key_source),
        key_file: proj.key_file.or(global.key_file),
    }
}

fn key_source_config(cfg: &protocol::SecretsConfig) -> KeySourceConfig {
    let kind = cfg
        .key_source
        .as_deref()
        .and_then(|s| crate::secret_keysource::KeySourceKind::parse(s).ok())
        .unwrap_or(crate::secret_keysource::KeySourceKind::Auto);
    KeySourceConfig {
        kind_override: Some(kind),
        // Treat an empty JSON string as absent. Otherwise `auto` selects the
        // device backend and later attempts to read an empty path as a key
        // file, obscuring the actionable "no key provided" error.
        key_file: cfg
            .key_file
            .as_deref()
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(PathBuf::from),
        // `passphrase` is env-driven for both the CLI and the daemon: the key
        // source reads ANIMUS_SECRET_PASSPHRASE at resolve time (and errors with
        // that instruction when unset), so there is no in-process passphrase to
        // thread through here. This keeps CLI and daemon behaviour identical and
        // script-safe — no TTY-only path that breaks under automation.
        passphrase: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret_keysource::tests::env_lock;
    use crate::secret_keysource::{KeySourceConfig, KeySourceKind};

    // A user-key store backed by a per-test key FILE, so tests need no shared
    // process env and never race each other.
    fn store_with_key(dir: &Path, key_name: &str, key: [u8; KEY_LEN]) -> DeviceEncryptedSecretStore {
        let kf = dir.join(key_name);
        std::fs::write(&kf, hex::encode(key)).unwrap();
        let cfg = KeySourceConfig { kind_override: Some(KeySourceKind::UserKey), key_file: Some(kf), passphrase: None };
        DeviceEncryptedSecretStore::new(dir.to_path_buf(), cfg)
    }
    fn store(dir: &Path) -> DeviceEncryptedSecretStore {
        store_with_key(dir, "test.key", [3u8; KEY_LEN])
    }

    #[test]
    fn round_trip_set_get_list_delete() {
        // UserKeySource::resolve checks ANIMUS_SECRET_KEY first; hold env_lock
        // so tests that mutate the var cannot race this key-file-based test.
        let _guard = env_lock().lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        assert_eq!(s.get("API_KEY").unwrap(), None);
        s.set("API_KEY", "sekret").unwrap();
        s.set("OTHER", "v2").unwrap();
        assert_eq!(s.get("API_KEY").unwrap().as_deref(), Some("sekret"));
        let mut keys = s.list_keys().unwrap();
        keys.sort();
        assert_eq!(keys, vec!["API_KEY".to_string(), "OTHER".to_string()]);
        assert!(s.delete("API_KEY").unwrap());
        assert!(!s.delete("API_KEY").unwrap());
        assert_eq!(s.get("API_KEY").unwrap(), None);
        assert_eq!(s.snapshot_for_spawn().unwrap().get("OTHER").map(String::as_str), Some("v2"));
    }

    #[test]
    fn file_is_not_plaintext() {
        let _guard = env_lock().lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        s.set("API_KEY", "PLAINTEXT_NEEDLE").unwrap();
        let raw = std::fs::read(s.path()).unwrap();
        assert!(!raw.windows(16).any(|w| w == b"PLAINTEXT_NEEDLE"), "secret value must not appear in the file");
    }

    #[test]
    fn tamper_fails_closed() {
        let _guard = env_lock().lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        s.set("API_KEY", "v").unwrap();
        let mut raw = std::fs::read(s.path()).unwrap();
        let n = raw.len();
        raw[n - 1] ^= 0xff; // flip a ciphertext byte
        std::fs::write(s.path(), &raw).unwrap();
        assert!(s.get("API_KEY").is_err(), "tampered store must fail closed");
    }

    #[test]
    fn wrong_device_key_cannot_decrypt() {
        let _guard = env_lock().lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        store_with_key(tmp.path(), "right.key", [3u8; KEY_LEN]).set("API_KEY", "v").unwrap();
        // Simulate the file moved to a machine with a different key.
        let wrong = store_with_key(tmp.path(), "wrong.key", [9u8; KEY_LEN]);
        assert!(wrong.get("API_KEY").is_err(), "a different device/user key must not decrypt the store");
    }

    // --- build_secret_store_for_project / merge / resolve_auto_backend ---

    fn write_project_secrets_config(project_root: &Path, key_source: Option<&str>, key_file: Option<&str>) {
        let animus_dir = project_root.join(".animus");
        std::fs::create_dir_all(&animus_dir).unwrap();
        let cfg = serde_json::json!({
            "secrets": {
                "key_source": key_source,
                "key_file": key_file
            }
        });
        std::fs::write(animus_dir.join("config.json"), serde_json::to_string_pretty(&cfg).unwrap()).unwrap();
    }

    #[test]
    fn merge_secrets_config_project_wins_field_by_field() {
        let global = protocol::SecretsConfig {
            backend: Some("keyring".to_string()),
            key_source: Some("device-id".to_string()),
            key_file: Some("/global/key".to_string()),
        };
        let project =
            Some(protocol::SecretsConfig { backend: None, key_source: Some("user-key".to_string()), key_file: None });
        let merged = merge_secrets_config(global, project);
        // project key_source wins; global backend/key_file kept where project has None
        assert_eq!(merged.key_source.as_deref(), Some("user-key"));
        assert_eq!(merged.backend.as_deref(), Some("keyring"));
        assert_eq!(merged.key_file.as_deref(), Some("/global/key"));
    }

    #[test]
    fn merge_secrets_config_no_project_returns_global() {
        let global = protocol::SecretsConfig {
            backend: Some("device".to_string()),
            key_source: Some("user-key".to_string()),
            key_file: Some("/k".to_string()),
        };
        let merged = merge_secrets_config(global.clone(), None);
        assert_eq!(merged, global);
    }

    #[test]
    fn has_server_key_configured_env_user_key() {
        use crate::secret_keysource::ENV_USER_KEY;
        let cfg = protocol::SecretsConfig::default();
        let _guard = env_lock().lock().unwrap();
        let prev = std::env::var(ENV_USER_KEY).ok();
        // Use a valid 32-byte hex key so other tests do not see an invalid value
        // if this key somehow outlives its lock window.
        std::env::set_var(ENV_USER_KEY, hex::encode([0xEEu8; KEY_LEN]));
        let result = has_server_key_configured(&cfg);
        match &prev {
            Some(v) => std::env::set_var(ENV_USER_KEY, v),
            None => std::env::remove_var(ENV_USER_KEY),
        }
        assert!(result, "has_server_key_configured must be true when ANIMUS_SECRET_KEY is set");
    }

    #[test]
    fn has_server_key_configured_via_config_key_source() {
        let cfg = protocol::SecretsConfig { key_source: Some("user-key".to_string()), ..Default::default() };
        assert!(has_server_key_configured(&cfg));
        let cfg_alias = protocol::SecretsConfig { key_source: Some("userkey".to_string()), ..Default::default() };
        assert!(has_server_key_configured(&cfg_alias));
        let cfg2 = protocol::SecretsConfig { key_source: Some("passphrase".to_string()), ..Default::default() };
        assert!(has_server_key_configured(&cfg2));
        let cfg3 = protocol::SecretsConfig { key_source: Some("device-id".to_string()), ..Default::default() };
        use crate::secret_keysource::tests::env_lock;
        use crate::secret_keysource::{ENV_PASSPHRASE, ENV_USER_KEY};
        let _guard = env_lock().lock().unwrap();
        let prev_key = std::env::var(ENV_USER_KEY).ok();
        let prev_pass = std::env::var(ENV_PASSPHRASE).ok();
        std::env::remove_var(ENV_USER_KEY);
        std::env::remove_var(ENV_PASSPHRASE);
        let result = has_server_key_configured(&cfg3);
        if let Some(v) = prev_key {
            std::env::set_var(ENV_USER_KEY, v)
        }
        if let Some(v) = prev_pass {
            std::env::set_var(ENV_PASSPHRASE, v)
        }
        assert!(!result, "device-id key source must not count as a server key");
    }

    #[test]
    fn has_server_key_configured_with_key_file() {
        use crate::secret_keysource::{ENV_PASSPHRASE, ENV_USER_KEY};
        let cfg =
            protocol::SecretsConfig { key_file: Some("/srv/animus/secret.key".to_string()), ..Default::default() };
        // Remove env vars so only key_file drives the result.
        let _guard = env_lock().lock().unwrap();
        let prev_key = std::env::var(ENV_USER_KEY).ok();
        let prev_pass = std::env::var(ENV_PASSPHRASE).ok();
        std::env::remove_var(ENV_USER_KEY);
        std::env::remove_var(ENV_PASSPHRASE);
        let result = has_server_key_configured(&cfg);
        if let Some(v) = prev_key {
            std::env::set_var(ENV_USER_KEY, v)
        }
        if let Some(v) = prev_pass {
            std::env::set_var(ENV_PASSPHRASE, v)
        }
        assert!(result, "key_file in secrets config must count as a server key source");
    }

    #[test]
    fn empty_key_file_is_not_a_server_key() {
        use crate::secret_keysource::{ENV_PASSPHRASE, ENV_USER_KEY};
        let cfg = protocol::SecretsConfig { key_file: Some("  ".to_string()), ..Default::default() };
        let _guard = env_lock().lock().unwrap();
        let prev_key = std::env::var(ENV_USER_KEY).ok();
        let prev_pass = std::env::var(ENV_PASSPHRASE).ok();
        std::env::remove_var(ENV_USER_KEY);
        std::env::remove_var(ENV_PASSPHRASE);

        let configured = has_server_key_configured(&cfg);
        let resolved = key_source_config(&cfg);

        match prev_key {
            Some(value) => std::env::set_var(ENV_USER_KEY, value),
            None => std::env::remove_var(ENV_USER_KEY),
        }
        match prev_pass {
            Some(value) => std::env::set_var(ENV_PASSPHRASE, value),
            None => std::env::remove_var(ENV_PASSPHRASE),
        }
        assert!(!configured, "an empty key_file must not select the device backend");
        assert!(resolved.key_file.is_none(), "an empty key_file must be normalized to absent");
    }

    #[test]
    fn key_file_path_is_trimmed_when_loaded_from_config() {
        let cfg = protocol::SecretsConfig {
            key_file: Some("  /srv/animus/secret.key\n".to_string()),
            ..Default::default()
        };

        let resolved = key_source_config(&cfg);

        assert_eq!(resolved.key_file.as_deref(), Some(Path::new("/srv/animus/secret.key")));
    }

    #[test]
    fn key_file_does_not_override_explicit_device_id_source() {
        use crate::secret_keysource::{ENV_PASSPHRASE, ENV_USER_KEY};
        let cfg = protocol::SecretsConfig {
            key_source: Some("device-id".to_string()),
            key_file: Some("/srv/animus/secret.key".to_string()),
            ..Default::default()
        };
        let _guard = env_lock().lock().unwrap();
        let prev_key = std::env::var(ENV_USER_KEY).ok();
        let prev_pass = std::env::var(ENV_PASSPHRASE).ok();
        std::env::remove_var(ENV_USER_KEY);
        std::env::remove_var(ENV_PASSPHRASE);
        let result = has_server_key_configured(&cfg);
        match prev_key {
            Some(value) => std::env::set_var(ENV_USER_KEY, value),
            None => std::env::remove_var(ENV_USER_KEY),
        }
        match prev_pass {
            Some(value) => std::env::set_var(ENV_PASSPHRASE, value),
            None => std::env::remove_var(ENV_PASSPHRASE),
        }
        assert!(!result, "key_file is ignored when device-id is explicitly selected");
    }

    #[test]
    fn empty_passphrase_env_is_not_a_server_key() {
        use crate::secret_keysource::{ENV_PASSPHRASE, ENV_USER_KEY};
        let _guard = env_lock().lock().unwrap();
        let prev_key = std::env::var(ENV_USER_KEY).ok();
        let prev_pass = std::env::var(ENV_PASSPHRASE).ok();
        std::env::remove_var(ENV_USER_KEY);
        std::env::set_var(ENV_PASSPHRASE, "   ");
        let result = has_server_key_configured(&protocol::SecretsConfig::default());
        match prev_key {
            Some(v) => std::env::set_var(ENV_USER_KEY, v),
            None => std::env::remove_var(ENV_USER_KEY),
        }
        match prev_pass {
            Some(v) => std::env::set_var(ENV_PASSPHRASE, v),
            None => std::env::remove_var(ENV_PASSPHRASE),
        }
        assert!(!result, "empty ANIMUS_SECRET_PASSPHRASE must be treated as unset");
    }

    #[test]
    fn build_secret_store_for_project_reads_project_config() {
        crate::test_env::stable_test_home();
        let _guard = env_lock().lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let project_dir = tmp.path().join("project");
        std::fs::create_dir_all(&project_dir).unwrap();
        let key = [0xABu8; KEY_LEN];
        let key_file = tmp.path().join("server.key");
        std::fs::write(&key_file, hex::encode(key)).unwrap();
        // Do not force `backend = device`: this exercises the production
        // headless path where configured server key material makes `auto`
        // select the device-encrypted store.
        write_project_secrets_config(&project_dir, Some("user-key"), Some(key_file.to_str().unwrap()));
        let scope = "test-project-scope";
        let scoped_root = tmp.path().join("state");
        std::fs::create_dir_all(&scoped_root).unwrap();
        // Ensure ANIMUS_SECRET_KEY is not set so the key file is used.
        use crate::secret_keysource::ENV_USER_KEY;
        let prev = std::env::var(ENV_USER_KEY).ok();
        std::env::remove_var(ENV_USER_KEY);
        let store = build_secret_store_for_project(scope, scoped_root, &project_dir);
        let set_result = store.set("FOO", "bar");
        let get_result = store.get("FOO");
        if let Some(v) = prev {
            std::env::set_var(ENV_USER_KEY, v)
        }
        set_result.expect("project-config-sourced store must accept writes");
        assert_eq!(get_result.unwrap().as_deref(), Some("bar"));
    }

    #[test]
    fn resolve_auto_backend_uses_device_when_server_key_in_cfg() {
        let cfg = protocol::SecretsConfig { backend: None, key_source: Some("user-key".to_string()), key_file: None };
        let dir = tempfile::tempdir().unwrap();
        // No pre-existing device store — but server key is configured.
        assert_eq!(resolve_auto_backend(&cfg, dir.path()), "device");
    }

    #[test]
    fn resolve_auto_backend_falls_back_to_keyring_without_server_key() {
        use crate::secret_keysource::tests::env_lock;
        use crate::secret_keysource::{ENV_PASSPHRASE, ENV_USER_KEY};
        let _guard = env_lock().lock().unwrap();
        let prev_key = std::env::var(ENV_USER_KEY).ok();
        let prev_pass = std::env::var(ENV_PASSPHRASE).ok();
        std::env::remove_var(ENV_USER_KEY);
        std::env::remove_var(ENV_PASSPHRASE);
        let cfg = protocol::SecretsConfig::default();
        let dir = tempfile::tempdir().unwrap();
        let result = resolve_auto_backend(&cfg, dir.path());
        if let Some(v) = prev_key {
            std::env::set_var(ENV_USER_KEY, v)
        }
        if let Some(v) = prev_pass {
            std::env::set_var(ENV_PASSPHRASE, v)
        }
        assert_eq!(result, "keyring", "auto without a server key and no existing store must fall back to keyring");
    }

    #[test]
    fn build_backend_for_project_honors_project_key_source() {
        crate::test_env::stable_test_home();
        let _guard = env_lock().lock().unwrap();
        use crate::secret_keysource::ENV_USER_KEY;
        let tmp = tempfile::tempdir().unwrap();
        let project_dir = tmp.path().join("project");
        std::fs::create_dir_all(&project_dir).unwrap();
        let key = [0xCDu8; KEY_LEN];
        let key_file = tmp.path().join("migrate.key");
        std::fs::write(&key_file, hex::encode(key)).unwrap();
        // Write project config with user-key source and a key_file.
        write_project_secrets_config(&project_dir, Some("user-key"), Some(key_file.to_str().unwrap()));
        let scope = "test-migrate-scope";
        let scoped_root = tmp.path().join("state");
        std::fs::create_dir_all(&scoped_root).unwrap();
        // Remove env var so only the key_file drives the device store key.
        let prev = std::env::var(ENV_USER_KEY).ok();
        std::env::remove_var(ENV_USER_KEY);
        let store = build_backend_for_project(scope, scoped_root, "device", &project_dir);
        let set_result = store.set("MIGRATE_KEY", "value");
        let get_result = store.get("MIGRATE_KEY");
        if let Some(v) = prev {
            std::env::set_var(ENV_USER_KEY, v)
        }
        set_result.expect("build_backend_for_project must honor project key_file for the device backend");
        assert_eq!(get_result.unwrap().as_deref(), Some("value"));
    }
}
