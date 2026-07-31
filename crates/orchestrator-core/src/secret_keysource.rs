//! Encryption-key sources for the device-encrypted secret store.
//!
//! A [`KeySource`] yields the 32-byte key that wraps the store's random master
//! key (see [`crate::secret_device_store`] and
//! `docs/architecture/secret-backends.md`). The wrapping key is never written to
//! disk by us — it is held by hardware / the OS, supplied by the operator, or
//! derived from device-bound material. Only the *wrapped* master key and the
//! AEAD-sealed secrets live in the store file.
//!
//! Cross-platform sources (`user-key`, `passphrase`, `device-id`) live here.
//! OS-hardware sources (Secure Enclave / DPAPI / TPM) are added behind the same
//! trait per platform.

use anyhow::{anyhow, bail, Context, Result};
use rand::RngCore;
use std::path::Path;
use zeroize::Zeroizing;

/// Length of the wrapping key (and the master key it wraps).
pub const KEY_LEN: usize = 32;

/// Env var holding a raw operator-supplied key (hex or base64), used by
/// `secret_key_source = user-key`.
pub const ENV_USER_KEY: &str = "ANIMUS_SECRET_KEY";
/// Env var holding a passphrase for `secret_key_source = passphrase` in
/// non-interactive (daemon) contexts.
pub const ENV_PASSPHRASE: &str = "ANIMUS_SECRET_PASSPHRASE";

/// A source of the 32-byte key that wraps the secret store's master key.
///
/// Implementations MUST be deterministic for a given device/config: the same
/// key must come back across process runs (and reboots, for the device
/// sources), or the store becomes undecryptable.
pub trait KeySource: Send + Sync {
    /// The wrapping key.
    fn key(&self) -> Result<Zeroizing<[u8; KEY_LEN]>>;
    /// Stable identifier recorded in the store header, so the store knows which
    /// source produced the wrap and can give an actionable error if the source
    /// later changes (e.g. the operator switched `secret_key_source`).
    fn id(&self) -> &'static str;
}

/// Which key source to use. Mirrors config `secret_key_source`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySourceKind {
    /// OS hardware key where available, else `device-id`.
    Auto,
    /// Operator-supplied raw key (env or file).
    UserKey,
    /// Argon2id over a passphrase.
    Passphrase,
    /// HKDF over the machine id + a per-install salt (binding only).
    DeviceId,
}

impl KeySourceKind {
    /// Parse the config string. Unknown values error rather than silently
    /// degrading — a misconfigured key source must not quietly change which key
    /// protects secrets.
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "user-key" | "user_key" | "userkey" => Ok(Self::UserKey),
            "passphrase" => Ok(Self::Passphrase),
            "device-id" | "device_id" | "deviceid" => Ok(Self::DeviceId),
            other => bail!("unknown secret_key_source '{other}' (expected: auto, user-key, passphrase, device-id)"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::UserKey => "user-key",
            Self::Passphrase => "passphrase",
            Self::DeviceId => "device-id",
        }
    }
}

/// Generate a fresh cryptographically-random salt (not secret; stored in the
/// store header so a derived key is reproducible across runs).
pub fn random_salt(len: usize) -> Vec<u8> {
    let mut salt = vec![0u8; len];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    salt
}

// ----------------------------------------------------------------------------
// user-key
// ----------------------------------------------------------------------------

/// Operator-supplied raw key: `ANIMUS_SECRET_KEY` (hex or base64) or a key file.
/// For headless / server hosts where a key is injected at deploy time.
pub struct UserKeySource {
    key: Zeroizing<[u8; KEY_LEN]>,
}

impl UserKeySource {
    /// Resolve from the env var first, then the configured key file.
    pub fn resolve(key_file: Option<&Path>) -> Result<Self> {
        if let Ok(raw) = std::env::var(ENV_USER_KEY) {
            if !raw.trim().is_empty() {
                return Ok(Self { key: parse_raw_key(raw.trim())? });
            }
        }
        if let Some(path) = key_file {
            let raw = std::fs::read_to_string(path)
                .with_context(|| format!("reading secrets.key_file at {}", path.display()))?;
            return Ok(Self { key: parse_raw_key(raw.trim())? });
        }
        bail!(
            "secret_key_source = user-key but no key provided: set {ENV_USER_KEY} (hex or base64, 32 bytes) or secrets.key_file"
        )
    }
}

impl KeySource for UserKeySource {
    fn key(&self) -> Result<Zeroizing<[u8; KEY_LEN]>> {
        Ok(self.key.clone())
    }
    fn id(&self) -> &'static str {
        "user-key"
    }
}

/// Decode a user-supplied key as hex (64 chars) or base64, requiring exactly
/// [`KEY_LEN`] bytes.
fn parse_raw_key(raw: &str) -> Result<Zeroizing<[u8; KEY_LEN]>> {
    use base64::Engine;
    let bytes = if raw.len() == KEY_LEN * 2 && raw.bytes().all(|b| b.is_ascii_hexdigit()) {
        hex::decode(raw).context("decoding ANIMUS_SECRET_KEY as hex")?
    } else {
        base64::engine::general_purpose::STANDARD
            .decode(raw)
            .context("decoding ANIMUS_SECRET_KEY as base64 (or hex)")?
    };
    if bytes.len() != KEY_LEN {
        bail!("secret key must be exactly {KEY_LEN} bytes ({} hex chars or base64), got {}", KEY_LEN * 2, bytes.len());
    }
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    key.copy_from_slice(&bytes);
    Ok(key)
}

// ----------------------------------------------------------------------------
// passphrase (Argon2id)
// ----------------------------------------------------------------------------

/// Argon2id key derivation from a passphrase. The salt is stored (not secret) in
/// the store header so the key is reproducible.
pub struct PassphraseKeySource {
    key: Zeroizing<[u8; KEY_LEN]>,
}

impl PassphraseKeySource {
    /// Derive from an explicit passphrase (callers may inject one) or, when
    /// `None`, the `ANIMUS_SECRET_PASSPHRASE` env var. Both the CLI and the
    /// daemon take the env path, so the mode is script-safe and behaves
    /// identically across surfaces.
    pub fn resolve(passphrase: Option<&str>, salt: &[u8]) -> Result<Self> {
        let owned;
        let pass = match passphrase {
            Some(p) => p,
            None => {
                owned = std::env::var(ENV_PASSPHRASE).map_err(|_| {
                    anyhow!("secret_key_source = passphrase but no passphrase available: set {ENV_PASSPHRASE}")
                })?;
                owned.as_str()
            }
        };
        if pass.is_empty() {
            bail!("secret passphrase must not be empty");
        }
        Self::derive(pass.as_bytes(), salt)
    }

    /// Run Argon2id over the passphrase + salt into a [`KEY_LEN`] key.
    pub fn derive(passphrase: &[u8], salt: &[u8]) -> Result<Self> {
        use argon2::{Algorithm, Argon2, Params, Version};
        let mut key = Zeroizing::new([0u8; KEY_LEN]);
        // Defaults are interactive-grade; deliberate, since the alternative
        // (no KDF) is far worse and we are not gating high-value remote auth.
        let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, Params::default());
        argon
            .hash_password_into(passphrase, salt, key.as_mut_slice())
            .map_err(|e| anyhow!("argon2 key derivation failed: {e}"))?;
        Ok(Self { key })
    }
}

impl KeySource for PassphraseKeySource {
    fn key(&self) -> Result<Zeroizing<[u8; KEY_LEN]>> {
        Ok(self.key.clone())
    }
    fn id(&self) -> &'static str {
        "passphrase"
    }
}

// ----------------------------------------------------------------------------
// device-id (binding only)
// ----------------------------------------------------------------------------

/// `HKDF-SHA256(ikm = machine id, salt = per-install salt)`. The machine id stays
/// on the device and never travels with the store file, so an off-device copy
/// cannot derive the key. The ids are readable on the live device, so this gives
/// device-binding but NOT on-device secrecy — preferred sources are the hardware
/// ones; this is the last-resort fallback.
pub struct DeviceIdKeySource {
    key: Zeroizing<[u8; KEY_LEN]>,
}

impl DeviceIdKeySource {
    /// Derive using the OS machine id and the provided per-install salt.
    pub fn resolve(salt: &[u8]) -> Result<Self> {
        let machine_id = machine_id().context("reading the OS machine id for the device-id key source")?;
        Self::derive(machine_id.as_bytes(), salt)
    }

    fn derive(machine_id: &[u8], salt: &[u8]) -> Result<Self> {
        use hkdf::Hkdf;
        use sha2::Sha256;
        let hk = Hkdf::<Sha256>::new(Some(salt), machine_id);
        let mut key = Zeroizing::new([0u8; KEY_LEN]);
        hk.expand(b"animus-device-secret-store-v1", key.as_mut_slice())
            .map_err(|e| anyhow!("HKDF expand failed: {e}"))?;
        Ok(Self { key })
    }
}

impl KeySource for DeviceIdKeySource {
    fn key(&self) -> Result<Zeroizing<[u8; KEY_LEN]>> {
        Ok(self.key.clone())
    }
    fn id(&self) -> &'static str {
        "device-id"
    }
}

/// Read a stable, device-unique id. Not secret; used only as HKDF input material
/// for the `device-id` source.
pub fn machine_id() -> Result<String> {
    #[cfg(target_os = "linux")]
    {
        for path in ["/etc/machine-id", "/var/lib/dbus/machine-id"] {
            if let Ok(s) = std::fs::read_to_string(path) {
                let s = s.trim().to_string();
                if !s.is_empty() {
                    return Ok(s);
                }
            }
        }
        bail!("no /etc/machine-id; set secret_key_source = user-key or passphrase on this host")
    }
    #[cfg(target_os = "macos")]
    {
        // IOPlatformUUID — stable per machine, readable without elevated rights.
        let out = std::process::Command::new("/usr/sbin/ioreg")
            .args(["-rd1", "-c", "IOPlatformExpertDevice"])
            .output()
            .context("running ioreg to read IOPlatformUUID")?;
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            if let Some(idx) = line.find("IOPlatformUUID") {
                if let Some(start) = line[idx..].find("= \"") {
                    let rest = &line[idx + start + 3..];
                    if let Some(end) = rest.find('"') {
                        return Ok(rest[..end].to_string());
                    }
                }
            }
        }
        bail!("could not parse IOPlatformUUID from ioreg")
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        // Windows machine id (registry MachineGuid) lands with the DPAPI work;
        // until then, require an explicit key source on unsupported targets.
        bail!("device-id key source not implemented on this OS; set secret_key_source = user-key or passphrase")
    }
}

/// Everything needed to (re)build a [`KeySource`] given the per-install salt the
/// store reads from / writes to its header.
#[derive(Clone, Default)]
pub struct KeySourceConfig {
    pub kind_override: Option<KeySourceKind>,
    pub key_file: Option<std::path::PathBuf>,
    pub passphrase: Option<Zeroizing<String>>,
}

impl KeySourceConfig {
    pub fn new(kind: KeySourceKind) -> Self {
        Self { kind_override: Some(kind), key_file: None, passphrase: None }
    }

    pub fn kind(&self) -> KeySourceKind {
        self.kind_override.unwrap_or(KeySourceKind::Auto)
    }
}

/// Build the configured key source given the per-install `salt` (used by the
/// salt-based sources; ignored by `user-key` and the hardware sources).
pub fn resolve_key_source(config: &KeySourceConfig, salt: &[u8]) -> Result<Box<dyn KeySource>> {
    match config.kind() {
        KeySourceKind::UserKey => Ok(Box::new(UserKeySource::resolve(config.key_file.as_deref())?)),
        KeySourceKind::Passphrase => {
            Ok(Box::new(PassphraseKeySource::resolve(config.passphrase.as_ref().map(|p| p.as_str()), salt)?))
        }
        KeySourceKind::DeviceId => Ok(Box::new(DeviceIdKeySource::resolve(salt)?)),
        KeySourceKind::Auto => resolve_auto(config, salt),
    }
}

/// `auto`: prefer operator-supplied server key material, then fall back to
/// `device-id`. Hardware providers (Secure Enclave / DPAPI / TPM) can be wired
/// in per platform; until a platform's provider lands, `auto` resolves per the
/// following priority:
///
/// 1. `ANIMUS_SECRET_KEY` env var → `user-key` (runtime-injected key; highest priority)
/// 2. `key_file` from `config` → `user-key` (operator-configured file; headless-safe)
/// 3. configured passphrase or `ANIMUS_SECRET_PASSPHRASE` env var → `passphrase`
///    (Argon2id KDF; headless-safe)
/// 4. `device-id` (fallback; interactive hosts only — binding, not on-device-secret-safe)
///
/// Steps 1–3 let headless/server deployments work without setting
/// `secret_key_source` explicitly: they just supply the key material (via env
/// or file) and `auto` does the right thing. This avoids the keyring-unavailable
/// hard error and prevents the device-id redeploy wipe caused by a new machine-id.
///
/// The priority here MUST mirror `has_server_key_configured` in
/// `secret_device_store` — that function picks the `device` backend for the
/// same set of conditions; if a condition triggers backend=device but this
/// function falls through to `device-id`, the store will be sealed with the
/// wrong key and reads will fail.
fn resolve_auto(config: &KeySourceConfig, salt: &[u8]) -> Result<Box<dyn KeySource>> {
    // Prefer operator-supplied key: env var wins over key_file so runtime
    // injection (e.g. Docker secrets via envFrom) takes precedence over a
    // file configured in the project/global config. If only key_file is set,
    // UserKeySource::resolve will still try the env first then the file.
    if std::env::var(ENV_USER_KEY).is_ok_and(|raw| !raw.trim().is_empty()) || config.key_file.is_some() {
        return Ok(Box::new(UserKeySource::resolve(config.key_file.as_deref())?));
    }
    // An in-process or env-injected passphrase is also a headless-safe server
    // source. Prefer the in-process value when the caller supplied one, just
    // as explicit user-key material takes precedence over its fallback.
    if config.passphrase.is_some() || std::env::var(ENV_PASSPHRASE).is_ok_and(|raw| !raw.trim().is_empty()) {
        return Ok(Box::new(PassphraseKeySource::resolve(
            config.passphrase.as_ref().map(|passphrase| passphrase.as_str()),
            salt,
        )?));
    }
    Ok(Box::new(DeviceIdKeySource::resolve(salt)?))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    /// Serialize all tests that mutate process-wide env vars so they cannot
    /// race each other. Any test that calls `set_var`/`remove_var` must hold
    /// this lock for the duration of the mutation + observation window.
    pub(crate) fn env_lock() -> &'static Mutex<()> {
        static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        ENV_LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn key_source_kind_parse_round_trips() {
        for k in [KeySourceKind::Auto, KeySourceKind::UserKey, KeySourceKind::Passphrase, KeySourceKind::DeviceId] {
            assert_eq!(KeySourceKind::parse(k.as_str()).unwrap(), k);
        }
        assert!(KeySourceKind::parse("nonsense").is_err());
    }

    #[test]
    fn user_key_accepts_hex_and_base64_and_rejects_wrong_length() {
        use base64::Engine;
        let raw = [7u8; KEY_LEN];
        let hex = hex::encode(raw);
        assert_eq!(*parse_raw_key(&hex).unwrap(), raw);
        let b64 = base64::engine::general_purpose::STANDARD.encode(raw);
        assert_eq!(*parse_raw_key(&b64).unwrap(), raw);
        assert!(parse_raw_key("deadbeef").is_err(), "16-bit key must be rejected");
        assert!(parse_raw_key("").is_err());
    }

    #[test]
    fn user_key_error_names_the_public_config_field() {
        let _guard = env_lock().lock().unwrap();
        let previous = std::env::var(ENV_USER_KEY).ok();
        std::env::remove_var(ENV_USER_KEY);
        let error = match UserKeySource::resolve(None) {
            Ok(_) => panic!("user-key resolution unexpectedly succeeded without key material"),
            Err(error) => error.to_string(),
        };
        match previous {
            Some(value) => std::env::set_var(ENV_USER_KEY, value),
            None => std::env::remove_var(ENV_USER_KEY),
        }
        assert!(error.contains("secrets.key_file"), "unexpected error: {error}");
    }

    #[test]
    fn passphrase_is_deterministic_per_salt_and_varies_by_salt() {
        let salt_a = [1u8; 16];
        let salt_b = [2u8; 16];
        let a1 = PassphraseKeySource::derive(b"correct horse", &salt_a).unwrap();
        let a2 = PassphraseKeySource::derive(b"correct horse", &salt_a).unwrap();
        let b = PassphraseKeySource::derive(b"correct horse", &salt_b).unwrap();
        assert_eq!(*a1.key().unwrap(), *a2.key().unwrap(), "same passphrase+salt must derive the same key");
        assert_ne!(*a1.key().unwrap(), *b.key().unwrap(), "different salt must derive a different key");
    }

    #[test]
    fn resolve_auto_uses_user_key_when_env_is_set() {
        use base64::Engine;
        let raw = [0x42u8; KEY_LEN];
        let b64 = base64::engine::general_purpose::STANDARD.encode(raw);
        let _guard = env_lock().lock().unwrap();
        let prev = std::env::var(ENV_USER_KEY).ok();
        std::env::set_var(ENV_USER_KEY, &b64);
        let salt = [0u8; 16];
        let result = resolve_auto(&KeySourceConfig::default(), &salt);
        match &prev {
            Some(v) => std::env::set_var(ENV_USER_KEY, v),
            None => std::env::remove_var(ENV_USER_KEY),
        }
        let src = result.expect("resolve_auto with ANIMUS_SECRET_KEY set should succeed");
        assert_eq!(src.id(), "user-key", "auto must resolve to user-key when ANIMUS_SECRET_KEY is set");
        assert_eq!(*src.key().unwrap(), raw);
    }

    #[test]
    fn resolve_auto_uses_user_key_when_key_file_configured() {
        let raw = [0x55u8; KEY_LEN];
        let tmp = tempfile::tempdir().unwrap();
        let key_file = tmp.path().join("server.key");
        std::fs::write(&key_file, hex::encode(raw)).unwrap();
        let _guard = env_lock().lock().unwrap();
        let prev = std::env::var(ENV_USER_KEY).ok();
        std::env::remove_var(ENV_USER_KEY);
        let config = KeySourceConfig { kind_override: None, key_file: Some(key_file), passphrase: None };
        let salt = [0u8; 16];
        let result = resolve_auto(&config, &salt);
        match &prev {
            Some(v) => std::env::set_var(ENV_USER_KEY, v),
            None => std::env::remove_var(ENV_USER_KEY),
        }
        let src = result.expect("resolve_auto with key_file configured should succeed");
        assert_eq!(src.id(), "user-key", "auto must resolve to user-key when key_file is configured");
        assert_eq!(*src.key().unwrap(), raw);
    }

    #[test]
    fn resolve_auto_prefers_env_user_key_over_configured_key_file() {
        let env_key = [0x66u8; KEY_LEN];
        let file_key = [0x77u8; KEY_LEN];
        let tmp = tempfile::tempdir().unwrap();
        let key_file = tmp.path().join("server.key");
        std::fs::write(&key_file, hex::encode(file_key)).unwrap();
        let _guard = env_lock().lock().unwrap();
        let prev = std::env::var(ENV_USER_KEY).ok();
        std::env::set_var(ENV_USER_KEY, hex::encode(env_key));
        let config = KeySourceConfig { kind_override: None, key_file: Some(key_file), passphrase: None };
        let salt = [0u8; 16];
        let result = resolve_auto(&config, &salt);
        match &prev {
            Some(v) => std::env::set_var(ENV_USER_KEY, v),
            None => std::env::remove_var(ENV_USER_KEY),
        }
        let src = result.expect("resolve_auto with both server key sources should succeed");
        assert_eq!(src.id(), "user-key");
        assert_eq!(*src.key().unwrap(), env_key, "ANIMUS_SECRET_KEY must override the configured key file");
    }

    #[test]
    fn resolve_auto_ignores_empty_env_user_key_when_key_file_configured() {
        let file_key = [0x78u8; KEY_LEN];
        let tmp = tempfile::tempdir().unwrap();
        let key_file = tmp.path().join("server.key");
        std::fs::write(&key_file, hex::encode(file_key)).unwrap();
        let _guard = env_lock().lock().unwrap();
        let prev = std::env::var(ENV_USER_KEY).ok();
        std::env::set_var(ENV_USER_KEY, "   ");
        let config = KeySourceConfig { kind_override: None, key_file: Some(key_file), passphrase: None };
        let salt = [0u8; 16];
        let result = resolve_auto(&config, &salt);
        match &prev {
            Some(v) => std::env::set_var(ENV_USER_KEY, v),
            None => std::env::remove_var(ENV_USER_KEY),
        }
        let src = result.expect("empty ANIMUS_SECRET_KEY must not mask a configured key file");
        assert_eq!(src.id(), "user-key");
        assert_eq!(*src.key().unwrap(), file_key);
    }

    #[test]
    fn resolve_auto_uses_passphrase_when_passphrase_env_is_set() {
        let _guard = env_lock().lock().unwrap();
        let prev_key = std::env::var(ENV_USER_KEY).ok();
        let prev_pass = std::env::var(ENV_PASSPHRASE).ok();
        std::env::remove_var(ENV_USER_KEY);
        std::env::set_var(ENV_PASSPHRASE, "headless-passphrase");
        let salt = [0xAAu8; 16];
        let result = resolve_auto(&KeySourceConfig::default(), &salt);
        match &prev_key {
            Some(v) => std::env::set_var(ENV_USER_KEY, v),
            None => std::env::remove_var(ENV_USER_KEY),
        }
        match &prev_pass {
            Some(v) => std::env::set_var(ENV_PASSPHRASE, v),
            None => std::env::remove_var(ENV_PASSPHRASE),
        }
        let src = result.expect("resolve_auto with ANIMUS_SECRET_PASSPHRASE set should succeed");
        assert_eq!(src.id(), "passphrase", "auto must resolve to passphrase when ANIMUS_SECRET_PASSPHRASE is set");
    }

    #[test]
    fn resolve_auto_ignores_empty_passphrase_env() {
        let _guard = env_lock().lock().unwrap();
        let prev_key = std::env::var(ENV_USER_KEY).ok();
        let prev_pass = std::env::var(ENV_PASSPHRASE).ok();
        std::env::remove_var(ENV_USER_KEY);
        std::env::set_var(ENV_PASSPHRASE, "   ");
        let salt = [0xACu8; 16];
        let result = resolve_auto(&KeySourceConfig::default(), &salt);
        match &prev_key {
            Some(v) => std::env::set_var(ENV_USER_KEY, v),
            None => std::env::remove_var(ENV_USER_KEY),
        }
        match &prev_pass {
            Some(v) => std::env::set_var(ENV_PASSPHRASE, v),
            None => std::env::remove_var(ENV_PASSPHRASE),
        }
        match result {
            Ok(src) => assert_eq!(src.id(), "device-id"),
            Err(err) => {
                let message = format!("{err:#}");
                assert!(
                    message.contains("machine id") || message.contains("machine-id"),
                    "empty passphrase must fall through to device-id, got: {message}"
                );
            }
        }
    }

    #[test]
    fn resolve_auto_uses_configured_passphrase_without_env() {
        let _guard = env_lock().lock().unwrap();
        let prev_key = std::env::var(ENV_USER_KEY).ok();
        let prev_pass = std::env::var(ENV_PASSPHRASE).ok();
        std::env::remove_var(ENV_USER_KEY);
        std::env::remove_var(ENV_PASSPHRASE);
        let config = KeySourceConfig {
            kind_override: None,
            key_file: None,
            passphrase: Some(Zeroizing::new("configured-headless-passphrase".to_string())),
        };
        let salt = [0xABu8; 16];
        let result = resolve_auto(&config, &salt);
        match &prev_key {
            Some(v) => std::env::set_var(ENV_USER_KEY, v),
            None => std::env::remove_var(ENV_USER_KEY),
        }
        match &prev_pass {
            Some(v) => std::env::set_var(ENV_PASSPHRASE, v),
            None => std::env::remove_var(ENV_PASSPHRASE),
        }
        let src = result.expect("resolve_auto with a configured passphrase should succeed");
        assert_eq!(src.id(), "passphrase");
    }

    #[test]
    fn resolve_auto_user_key_wins_over_passphrase() {
        use base64::Engine;
        let raw = [0x99u8; KEY_LEN];
        let b64 = base64::engine::general_purpose::STANDARD.encode(raw);
        let _guard = env_lock().lock().unwrap();
        let prev_key = std::env::var(ENV_USER_KEY).ok();
        let prev_pass = std::env::var(ENV_PASSPHRASE).ok();
        std::env::set_var(ENV_USER_KEY, &b64);
        std::env::set_var(ENV_PASSPHRASE, "also-set");
        let salt = [0u8; 16];
        let result = resolve_auto(&KeySourceConfig::default(), &salt);
        match &prev_key {
            Some(v) => std::env::set_var(ENV_USER_KEY, v),
            None => std::env::remove_var(ENV_USER_KEY),
        }
        match &prev_pass {
            Some(v) => std::env::set_var(ENV_PASSPHRASE, v),
            None => std::env::remove_var(ENV_PASSPHRASE),
        }
        let src = result.expect("resolve_auto with both env vars set should succeed");
        assert_eq!(src.id(), "user-key", "user-key env must take priority over passphrase env");
    }

    #[test]
    fn device_id_is_deterministic_and_binds_to_machine_material() {
        let salt = [9u8; 16];
        let k1 = DeviceIdKeySource::derive(b"machine-AAAA", &salt).unwrap();
        let k2 = DeviceIdKeySource::derive(b"machine-AAAA", &salt).unwrap();
        let other = DeviceIdKeySource::derive(b"machine-BBBB", &salt).unwrap();
        assert_eq!(*k1.key().unwrap(), *k2.key().unwrap());
        assert_ne!(*k1.key().unwrap(), *other.key().unwrap(), "a different machine must derive a different key");
    }
}
