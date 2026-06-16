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
        std::fs::rename(&tmp, &self.secrets_path).map_err(|e| io_err(&self.secrets_path, e))?;
        Ok(())
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
/// `backend = device`, or when an encrypted store already exists for this scope
/// (a migrated install keeps using it). This is the single seam the rest of the
/// codebase constructs through, replacing direct `KeyringSecretStore::new`.
pub fn build_secret_store(repo_scope: &str, scoped_root: impl Into<PathBuf>) -> Box<dyn SecretStore> {
    let scoped_root = scoped_root.into();
    let cfg = protocol::Config::load_global_if_exists().and_then(|c| c.secrets).unwrap_or_default();
    let key_config = key_source_config(&cfg);
    let backend = cfg.backend.as_deref().unwrap_or("auto");

    let device = DeviceEncryptedSecretStore::new(scoped_root.clone(), key_config);
    let use_device = match backend {
        "device" => true,
        "keyring" | "env" => false,
        // auto: keep using the device store once one exists (post-migration),
        // otherwise stay on the keyring so existing secrets are never stranded.
        _ => device.path().exists(),
    };
    if use_device {
        Box::new(device)
    } else {
        Box::new(crate::secret_store::KeyringSecretStore::new(repo_scope, scoped_root))
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
        key_file: cfg.key_file.as_ref().map(PathBuf::from),
        // CLI supplies a prompt-derived passphrase explicitly; the daemon reads
        // ANIMUS_SECRET_PASSPHRASE at resolve time, so None is correct here.
        passphrase: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        s.set("API_KEY", "PLAINTEXT_NEEDLE").unwrap();
        let raw = std::fs::read(s.path()).unwrap();
        assert!(!raw.windows(16).any(|w| w == b"PLAINTEXT_NEEDLE"), "secret value must not appear in the file");
    }

    #[test]
    fn tamper_fails_closed() {
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
        let tmp = tempfile::tempdir().unwrap();
        store_with_key(tmp.path(), "right.key", [3u8; KEY_LEN]).set("API_KEY", "v").unwrap();
        // Simulate the file moved to a machine with a different key.
        let wrong = store_with_key(tmp.path(), "wrong.key", [9u8; KEY_LEN]);
        assert!(wrong.get("API_KEY").is_err(), "a different device/user key must not decrypt the store");
    }
}
