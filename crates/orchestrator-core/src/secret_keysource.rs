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
use std::sync::atomic::{AtomicBool, Ordering};
use zeroize::Zeroizing;

/// Length of the wrapping key (and the master key it wraps).
pub const KEY_LEN: usize = 32;

/// Env var holding a raw operator-supplied key (hex or base64), used by
/// `secret_key_source = user-key`.
pub const ENV_USER_KEY: &str = "ANIMUS_SECRET_KEY";
/// Env var staging the NEXT operator key for `animus secret rewrap-key`: the
/// store's master key is re-wrapped from the current key to this one, then the
/// operator swaps ANIMUS_SECRET_KEY to the new value and unsets this var.
pub const ENV_USER_KEY_NEXT: &str = "ANIMUS_SECRET_KEY_NEXT";
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
            return Ok(Self { key: parse_raw_key(raw.trim())? });
        }
        if let Some(path) = key_file {
            let raw = std::fs::read_to_string(path)
                .with_context(|| format!("reading secret_key_file at {}", path.display()))?;
            return Ok(Self { key: parse_raw_key(raw.trim())? });
        }
        bail!(
            "secret_key_source = user-key but no key provided: set {ENV_USER_KEY} (hex or base64, 32 bytes) or secret_key_file"
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

/// Resolve the staged NEXT operator key for `animus secret rewrap-key`
/// ([`ENV_USER_KEY_NEXT`], hex or base64, 32 bytes). The rotation flow:
/// stage the new key here, run `animus secret rewrap-key`, verify, swap
/// [`ENV_USER_KEY`] to the new value, unset this var.
pub fn resolve_next_user_key() -> Result<Zeroizing<[u8; KEY_LEN]>> {
    let raw = std::env::var(ENV_USER_KEY_NEXT)
        .with_context(|| format!("{ENV_USER_KEY_NEXT} is not set (hex or base64, 32 bytes)"))?;
    parse_raw_key(raw.trim())
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

/// The material `auto` selects, after weighing available key sources against the
/// execution context. A pure decision so it is deterministically testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutoDecision {
    /// Real operator-supplied key material is available (`ANIMUS_SECRET_KEY` or
    /// a configured key file).
    UserKey,
    /// A passphrase is available (`ANIMUS_SECRET_PASSPHRASE`).
    Passphrase,
    /// No real key material: bind to the (locally-decryptable) device id.
    DeviceId,
    /// No real key material on a headless/server host: refuse rather than
    /// silently protect secrets with a device key any local user can rederive.
    HardError,
}

/// One-shot guard so the device-id fallback warning fires at most once per
/// process instead of on every secret read/write.
static DEVICE_ID_FALLBACK_WARNED: AtomicBool = AtomicBool::new(false);

/// Pure `auto` selection: prefer real key material, then a passphrase, then fall
/// back to `device-id` — but only on an interactive local host. On a
/// server/headless host (no TTY or `ANIMUS_SERVER=1`) with no real key material,
/// refuse instead of using the locally-decryptable device key.
// The four booleans are the deliberate pure-decision seam: each captures an
// independent, orthogonal input (key material, passphrase, host posture, TTY)
// so the precedence can be exhaustively unit-tested without touching real
// env/TTY. Collapsing them into enums would obscure the truth table.
#[allow(clippy::fn_params_excessive_bools)]
fn decide_auto(has_user_key: bool, has_passphrase: bool, is_server: bool, has_tty: bool) -> AutoDecision {
    if has_user_key {
        AutoDecision::UserKey
    } else if has_passphrase {
        AutoDecision::Passphrase
    } else if is_server || !has_tty {
        AutoDecision::HardError
    } else {
        AutoDecision::DeviceId
    }
}

/// True when this process looks like a server/headless host: `ANIMUS_SERVER=1`
/// is set explicitly.
fn is_server_env() -> bool {
    std::env::var("ANIMUS_SERVER").map(|v| v.trim() == "1").unwrap_or(false)
}

/// True when this process looks interactive: any of stdin/stdout/stderr is a
/// terminal. Checking all three (not just stdin) keeps supported local flows
/// like `printf token | animus secret set KEY` working — piping stdin from an
/// interactive shell still leaves stdout/stderr on the TTY. A fully-detached
/// server/daemon process has none of the three on a terminal and is treated as
/// headless.
fn has_interactive_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() || std::io::stdout().is_terminal() || std::io::stderr().is_terminal()
}

/// `auto`: prefer real operator key material (`ANIMUS_SECRET_KEY` / key file),
/// then a passphrase (`ANIMUS_SECRET_PASSPHRASE`), and only otherwise fall back
/// to the device-id source. The device-id fallback is device-binding but NOT
/// on-device secrecy (any local user can rederive the key), so it is allowed
/// only on interactive local hosts (with a one-time warning) and is a hard error
/// on server/headless hosts.
///
/// Hardware providers (Secure Enclave / DPAPI / TPM) will slot in ahead of the
/// device-id fallback per platform as they land.
fn resolve_auto(config: &KeySourceConfig, salt: &[u8]) -> Result<Box<dyn KeySource>> {
    let has_user_key = std::env::var_os(ENV_USER_KEY).is_some() || config.key_file.is_some();
    let has_passphrase = config.passphrase.is_some() || std::env::var_os(ENV_PASSPHRASE).is_some();
    match decide_auto(has_user_key, has_passphrase, is_server_env(), has_interactive_tty()) {
        AutoDecision::UserKey => Ok(Box::new(UserKeySource::resolve(config.key_file.as_deref())?)),
        AutoDecision::Passphrase => {
            Ok(Box::new(PassphraseKeySource::resolve(config.passphrase.as_ref().map(|p| p.as_str()), salt)?))
        }
        AutoDecision::DeviceId => {
            if !DEVICE_ID_FALLBACK_WARNED.swap(true, Ordering::Relaxed) {
                tracing::warn!(
                    "secret_key_source = auto fell back to device-id: the secret store is bound to \
                     this machine but is decryptable by any local user (the device id is not secret). \
                     Set {ENV_USER_KEY} (hex/base64 32-byte key), a secret_key_file, or {ENV_PASSPHRASE} \
                     for real at-rest secrecy."
                );
            }
            Ok(Box::new(DeviceIdKeySource::resolve(salt)?))
        }
        AutoDecision::HardError => bail!(
            "secret_key_source = auto has no key material on a server/headless host, and the \
             device-id fallback is decryptable by any local user. Provide real key material: set \
             {ENV_USER_KEY} (hex/base64 32-byte key), configure secret_key_file, or set {ENV_PASSPHRASE}."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn decide_auto_prefers_user_key_then_passphrase() {
        // Real key material always wins, regardless of host/TTY.
        for &is_server in &[false, true] {
            for &has_tty in &[false, true] {
                assert_eq!(decide_auto(true, false, is_server, has_tty), AutoDecision::UserKey);
                assert_eq!(decide_auto(true, true, is_server, has_tty), AutoDecision::UserKey);
            }
        }
        // Passphrase wins when no user key is present.
        for &is_server in &[false, true] {
            for &has_tty in &[false, true] {
                assert_eq!(decide_auto(false, true, is_server, has_tty), AutoDecision::Passphrase);
            }
        }
    }

    #[test]
    fn decide_auto_falls_back_to_device_id_only_when_interactive() {
        // Neither key nor passphrase, interactive local host with a TTY.
        assert_eq!(decide_auto(false, false, false, true), AutoDecision::DeviceId);
    }

    #[test]
    fn decide_auto_hard_errors_on_server_or_no_tty() {
        // Explicit server marker.
        assert_eq!(decide_auto(false, false, true, true), AutoDecision::HardError);
        // No TTY (headless).
        assert_eq!(decide_auto(false, false, false, false), AutoDecision::HardError);
        // Both.
        assert_eq!(decide_auto(false, false, true, false), AutoDecision::HardError);
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
