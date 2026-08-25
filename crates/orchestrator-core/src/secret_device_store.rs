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
        use std::io::Write;
        let dir = self.secrets_path.parent().expect("secrets path has a parent");
        std::fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
        restrict_dir(dir)?;

        // Create the temp file with O_EXCL + 0600 (unix) so the ciphertext is
        // never world-readable, even for the brief window between write and
        // chmod, and a pre-created path (regular file or symlink) cannot be
        // written through. Retry on collision with a fresh random name.
        let mut attempts = 0u32;
        let tmp = loop {
            let candidate = dir.join(format!(".secrets.{}.{:x}.tmp", std::process::id(), rand::rngs::OsRng.next_u64()));
            match create_new_restricted(&candidate) {
                Ok(mut f) => {
                    f.write_all(bytes).map_err(|e| {
                        let _ = std::fs::remove_file(&candidate);
                        io_err(&candidate, e)
                    })?;
                    f.sync_all().map_err(|e| {
                        let _ = std::fs::remove_file(&candidate);
                        io_err(&candidate, e)
                    })?;
                    break candidate;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && attempts < 8 => {
                    attempts += 1;
                    continue;
                }
                Err(e) => return Err(io_err(&candidate, e)),
            }
        };

        // POSIX `rename` atomically replaces an existing destination; Windows
        // `rename` errors if the destination exists, so fall back to removing
        // the old file first (the advisory write lock serializes this).
        match std::fs::rename(&tmp, &self.secrets_path) {
            Ok(()) => Ok(()),
            Err(_) if self.secrets_path.exists() => {
                std::fs::remove_file(&self.secrets_path).map_err(|e| {
                    let _ = std::fs::remove_file(&tmp);
                    io_err(&self.secrets_path, e)
                })?;
                std::fs::rename(&tmp, &self.secrets_path).map_err(|e| {
                    let _ = std::fs::remove_file(&tmp);
                    io_err(&self.secrets_path, e)
                })
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                Err(io_err(&self.secrets_path, e))
            }
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

    /// Parse only the header, returning (source_id, salt, wrapped_master,
    /// offset of the sealed body). Shared by `decode` and `rewrap_master_key`.
    fn parse_header(raw: &[u8]) -> SecretStoreResult<(String, Vec<u8>, Vec<u8>, usize)> {
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
        Ok((source_id, salt, wrapped_master, cur.pos))
    }

    /// Read-only probe for deploy preflights (`animus secret verify`): unlocks
    /// the store exactly like a real read but NEVER creates, initializes, or
    /// rewrites anything — a wrong key can never silently wipe the store.
    pub fn verify(&self) -> SecretVerifyStatus {
        let raw = match std::fs::read(&self.secrets_path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return SecretVerifyStatus::Missing,
            Err(e) => return SecretVerifyStatus::Corrupt(format!("read failed: {e}")),
        };
        match self.decode(&raw) {
            Ok(state) => SecretVerifyStatus::Ok { entries: state.entries.len() },
            Err(err) => {
                let msg = err.to_string();
                // AEAD failure means the ciphertext did not authenticate under
                // the configured key (wrong key or tamper); a key-source id
                // mismatch means the operator pointed at the wrong source.
                // Both are "unlock" problems, not structural corruption.
                if msg.contains("AEAD verification failed") || msg.contains("was written with key source") {
                    SecretVerifyStatus::UnlockFailed(msg)
                } else {
                    SecretVerifyStatus::Corrupt(msg)
                }
            }
        }
    }

    /// Re-wrap the store's master key under `new_wrap_key` (rotation of the
    /// operator key, e.g. a new ANIMUS_SECRET_KEY). Only the wrap changes: the
    /// master key and every sealed secret stay byte-identical, so no secret
    /// re-encryption is needed. Serialized under the same advisory write lock
    /// as every other mutation; fails WITHOUT touching the file when the
    /// current key cannot unlock the store first.
    ///
    /// Idempotent: when the file already unwraps to the same master key under
    /// the new key (a retried rotation), reports AlreadyWrapped and succeeds.
    pub fn rewrap_master_key(&self, new_wrap_key: &[u8; KEY_LEN]) -> SecretStoreResult<RewrapOutcome> {
        self.with_write_lock(|| {
            let raw = std::fs::read(&self.secrets_path).map_err(|e| io_err(&self.secrets_path, e))?;
            let (source_id, salt, wrapped_master, body_offset) = Self::parse_header(&raw)?;
            let aad = Self::aad(&source_id, &salt);

            // Try CURRENT first, then NEXT (codex review): after a completed
            // rotation the configured key is stale until the operator swaps
            // env, and a retried rewrap must still succeed idempotently.
            let state = match self.decode(&raw) {
                Ok(state) => state,
                Err(decode_err) => {
                    if let Ok(recovered) = open(new_wrap_key, &wrapped_master, &aad) {
                        if recovered.len() == KEY_LEN {
                            return Ok(RewrapOutcome::AlreadyWrapped);
                        }
                    }
                    return Err(decode_err);
                }
            };

            // Idempotency: already wrapped under the new key to the same master.
            if let Ok(recovered) = open(new_wrap_key, &wrapped_master, &aad) {
                if recovered.as_slice() == state.master_key.as_slice() {
                    return Ok(RewrapOutcome::AlreadyWrapped);
                }
                return Err(backend(
                    "new key unwraps the store but recovers a DIFFERENT master key — refusing to rotate (possible key collision or tamper)",
                ));
            }

            let new_wrapped = seal(new_wrap_key, state.master_key.as_slice(), &aad)?;
            // Self-check before committing: the new wrap must recover the exact
            // master key (codex review: unwrap → wrap → unwrap → compare).
            let recovered = open(new_wrap_key, &new_wrapped, &aad)?;
            if recovered.as_slice() != state.master_key.as_slice() {
                return Err(backend("post-rotation unwrap mismatch; the store was NOT rewritten"));
            }

            let mut out = Vec::with_capacity(raw.len());
            out.extend_from_slice(MAGIC);
            out.push(FORMAT_VERSION);
            write_lp(&mut out, source_id.as_bytes());
            write_lp(&mut out, &state.salt);
            write_lp(&mut out, &new_wrapped);
            out.extend_from_slice(&raw[body_offset..]);
            self.atomic_write(&out)?;
            Ok(RewrapOutcome::Rewrapped)
        })
    }
}

/// Outcome of a read-only `verify` probe. Never mutates the store.
#[derive(Debug)]
pub enum SecretVerifyStatus {
    /// Store decrypted cleanly; carries the entry count.
    Ok { entries: usize },
    /// No store file exists (fresh install; a later write initializes it).
    Missing,
    /// The file exists but cannot be unlocked with the configured key source
    /// (wrong key, wrong key-source configuration, or tampered ciphertext).
    UnlockFailed(String),
    /// The file is structurally invalid (bad magic/version/framing).
    Corrupt(String),
}

/// Outcome of [`DeviceEncryptedSecretStore::rewrap_master_key`].
#[derive(Debug, PartialEq, Eq)]
pub enum RewrapOutcome {
    /// The master key was re-wrapped under the new key and the store rewritten.
    Rewrapped,
    /// The store was already wrapped under the new key — idempotent success.
    AlreadyWrapped,
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

/// Open a fresh temp file exclusively (`O_EXCL`) with owner-only `0600` bits on
/// unix, so the ciphertext is never even briefly world-readable and a
/// pre-created path (regular file or symlink) cannot be written through.
#[cfg(unix)]
fn create_new_restricted(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new().create_new(true).write(true).mode(0o600).open(path)
}
#[cfg(not(unix))]
fn create_new_restricted(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new().create_new(true).write(true).open(path)
}

#[cfg(unix)]
fn restrict_dir(path: &Path) -> SecretStoreResult<()> {
    use std::os::unix::fs::PermissionsExt;
    // Do not swallow the chmod failure: a secrets dir left group/world-readable
    // is a real exposure, so surface it rather than silently continuing.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(|e| io_err(path, e))
}
#[cfg(not(unix))]
fn restrict_dir(_path: &Path) -> SecretStoreResult<()> {
    Ok(())
}

/// Build the configured [`SecretStore`] for a repo scope. Reads the global
/// `secrets` config to choose the backend. Conservative default: the OS keyring
/// (existing installs are unchanged). Uses the device-encrypted store when
/// `backend = device`, or when an encrypted store already exists for this scope
/// (a migrated install keeps using it). This is the single seam the rest of the
/// codebase constructs through, replacing direct `KeyringSecretStore::new`.
pub fn build_secret_store(repo_scope: &str, scoped_root: impl Into<PathBuf>) -> Box<dyn SecretStore> {
    let scoped_root = scoped_root.into();
    let cfg = protocol::Config::load_global_if_exists().and_then(|c| c.secrets).unwrap_or_default();
    let resolved = match cfg.backend.as_deref().unwrap_or("auto") {
        "device" => "device",
        "keyring" | "env" => "keyring",
        // auto: keep using the device store once one exists (post-migration),
        // otherwise stay on the keyring so existing secrets are never stranded.
        _ => {
            let device = DeviceEncryptedSecretStore::new(scoped_root.clone(), key_source_config(&cfg));
            if device.path().exists() {
                "device"
            } else {
                "keyring"
            }
        }
    };
    build_backend(repo_scope, scoped_root, resolved)
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

fn key_source_config(cfg: &protocol::SecretsConfig) -> KeySourceConfig {
    let kind = cfg
        .key_source
        .as_deref()
        .and_then(|s| crate::secret_keysource::KeySourceKind::parse(s).ok())
        .unwrap_or(crate::secret_keysource::KeySourceKind::Auto);
    KeySourceConfig {
        kind_override: Some(kind),
        key_file: cfg.key_file.as_ref().map(PathBuf::from),
        // `passphrase` is env-driven for both the CLI and the daemon: the key
        // source reads ANIMUS_SECRET_PASSPHRASE at resolve time (and errors with
        // that instruction when unset), so there is no in-process passphrase to
        // thread through here. This keeps CLI and daemon behaviour identical and
        // script-safe — no TTY-only path that breaks under automation.
        passphrase: None,
    }
}

/// Build the device-encrypted store for a repo scope with the global `secrets`
/// config's key source, mirroring `build_secret_store`'s resolution. Used by
/// `animus secret verify` / `animus secret rewrap-key`, which must address the
/// encrypted store directly regardless of the configured default backend.
pub fn build_device_store(scoped_root: impl Into<PathBuf>) -> DeviceEncryptedSecretStore {
    let scoped_root = scoped_root.into();
    let cfg = protocol::Config::load_global_if_exists().and_then(|c| c.secrets).unwrap_or_default();
    DeviceEncryptedSecretStore::new(scoped_root, key_source_config(&cfg))
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

    // --- verify / rewrap_master_key (operator-key rotation) ---

    #[test]
    fn verify_reports_missing_ok_unlock_failed_and_corrupt() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        assert!(matches!(s.verify(), SecretVerifyStatus::Missing));

        s.set("API_KEY", "v").unwrap();
        assert!(matches!(s.verify(), SecretVerifyStatus::Ok { entries: 1 }));

        let wrong = store_with_key(tmp.path(), "wrong.key", [9u8; KEY_LEN]);
        assert!(matches!(wrong.verify(), SecretVerifyStatus::UnlockFailed(_)));

        std::fs::write(s.path(), b"garbage").unwrap();
        assert!(matches!(s.verify(), SecretVerifyStatus::Corrupt(_)));
    }

    #[test]
    fn rewrap_rotates_the_wrap_key_without_touching_secrets() {
        let tmp = tempfile::tempdir().unwrap();
        let key_a = [3u8; KEY_LEN];
        let key_b = [7u8; KEY_LEN];
        let store_a = store_with_key(tmp.path(), "a.key", key_a);
        store_a.set("API_KEY", "sekret").unwrap();
        store_a.set("OTHER", "v2").unwrap();

        assert_eq!(store_a.rewrap_master_key(&key_b).unwrap(), RewrapOutcome::Rewrapped);
        // Idempotent: a retried rotation is a no-op success.
        assert_eq!(store_a.rewrap_master_key(&key_b).unwrap(), RewrapOutcome::AlreadyWrapped);

        // The new key reads everything; the old key no longer unlocks.
        let store_b = store_with_key(tmp.path(), "b.key", key_b);
        assert!(matches!(store_b.verify(), SecretVerifyStatus::Ok { entries: 2 }));
        assert_eq!(store_b.get("API_KEY").unwrap().as_deref(), Some("sekret"));
        assert_eq!(store_b.get("OTHER").unwrap().as_deref(), Some("v2"));
        assert!(matches!(store_a.verify(), SecretVerifyStatus::UnlockFailed(_)));
    }

    #[test]
    fn rewrap_refuses_a_new_key_that_recovers_a_different_master() {
        // Construct the pathological case by hand: a store whose wrapped blob
        // authenticates under BOTH the old key and a colliding new key is not
        // constructible via the public API, so this test instead proves the
        // guard's inverse: rotation to the CURRENT key is idempotent, never a
        // rewrite.
        let tmp = tempfile::tempdir().unwrap();
        let key_a = [3u8; KEY_LEN];
        let store_a = store_with_key(tmp.path(), "a.key", key_a);
        store_a.set("API_KEY", "sekret").unwrap();
        let before = std::fs::read(store_a.path()).unwrap();
        assert_eq!(store_a.rewrap_master_key(&key_a).unwrap(), RewrapOutcome::AlreadyWrapped);
        let after = std::fs::read(store_a.path()).unwrap();
        assert_eq!(before, after, "idempotent no-op must not rewrite the file");
    }

    #[test]
    fn rewrap_requires_the_current_key() {
        let tmp = tempfile::tempdir().unwrap();
        let store_a = store_with_key(tmp.path(), "a.key", [3u8; KEY_LEN]);
        store_a.set("API_KEY", "sekret").unwrap();
        // A store configured with the WRONG current key must fail closed and
        // leave the file byte-identical.
        let store_wrong = store_with_key(tmp.path(), "wrong.key", [9u8; KEY_LEN]);
        let before = std::fs::read(store_a.path()).unwrap();
        assert!(store_wrong.rewrap_master_key(&[7u8; KEY_LEN]).is_err());
        let after = std::fs::read(store_a.path()).unwrap();
        assert_eq!(before, after);
        assert!(matches!(store_a.verify(), SecretVerifyStatus::Ok { entries: 1 }));
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
