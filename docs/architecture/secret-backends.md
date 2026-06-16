# Secret storage backends (device-encrypted store + keyring)

Status: design (feat/device-encrypted-secrets). Implements a device-bound,
at-rest-encrypted `SecretStore` backend alongside the existing OS-keyring
backend, plus a configurable encryption-key source so headless / non-keychain
hosts work and operators can bring their own key.

## Why

The OS keyring (`KeyringSecretStore`) is strong on desktops but (1) re-prompts on
macOS when the calling binary's code signature is unstable, and (2) does not
exist on a headless Linux server (no session keyring/TPM guaranteed). A
device-encrypted file store fixes both: no per-binary keychain ACL, works
everywhere, and binds the ciphertext to the device.

## Threat model (honest boundaries)

- **Defeats off-device theft.** The encrypted file copied to another machine /
  an errant backup / a synced folder is useless — the key-source material is not
  in the file and is bound to this device (or supplied by the operator).
- **Raises the on-device bar** from "read a file" to "execute code on the live
  device as this user".
- **Does NOT defend against a live attacker running as the same user.** Anything
  Animus can do to unwrap the key, that attacker runs identically. No local
  software scheme can beat this; we do not pretend otherwise. Hardware key
  isolation (Secure Enclave/TPM/DPAPI) only raises the cost, it is not a wall.
- Tamper → AEAD verification fails closed. Key material lives in `Zeroizing`;
  secret values are never logged.

## Architecture (fits the existing `SecretStore` trait)

`SecretStore` (set/get/delete/list_keys/snapshot_for_spawn) stays the seam.

- New backend `DeviceEncryptedSecretStore: SecretStore`.
- A factory `build_secret_store(repo_scope, scoped_root) -> Box<dyn SecretStore>`
  reads config and returns the configured backend. It replaces the ~5 direct
  `KeyringSecretStore::new(&scope, scoped_root)` construction sites
  (orchestrator-daemon-runtime/quotas.rs ×2, animus-mcp-oauth/config.rs,
  orchestrator-cli ops_secret.rs, ops_doctor/checks_api_keys.rs).
- Resolution precedence is unchanged and already lives at
  `orchestrator-config .../env_interp.rs::lookup_env_then_secret_store`
  (explicit env wins, then the store). The plugin-spawn path injects via the
  installable `SecretSnapshotProvider` (plugin-host host.rs) over the same store.

## On-disk format — `<scoped_root>/secrets/secrets.enc.v1`

`0600`, written via atomic temp+rename. A versioned header followed by an
AEAD-sealed JSON map `{key: value}`:

- `master_key`: 32 random bytes, generated once, stored **wrapped** (AEAD-sealed
  under the KeySource key). Wrapping the master key lets us rotate the device /
  user key by re-wrapping only the master key, never re-encrypting all data.
- Data blob: the JSON secret map sealed with the master key.
- AEAD: ChaCha20-Poly1305 (RustCrypto `chacha20poly1305`) — portable, constant
  time, no AES-NI dependence. Fresh random 96-bit nonce per seal.
- Header carries: magic, format version, AEAD id, key-source id + params, salt,
  nonces. The header is authenticated as AAD.

## KeySource (wraps the master key)

```
trait KeySource { fn key(&self) -> Result<Zeroizing<[u8; 32]>>; fn id(&self) -> &str; }
```

Selected by config `secret_key_source`:

- `auto` (default): OS device key — Secure Enclave (macOS) / DPAPI (Windows) /
  TPM (Linux) → falls back to `device-id` when no hardware path exists.
- `user-key`: operator-supplied 32-byte key from `ANIMUS_SECRET_KEY`
  (hex or base64) or a `secret_key_file` path. For headless/server with a
  deploy-injected key (systemd `LoadCredential`, mounted secret, external KMS).
- `passphrase`: `Argon2id(passphrase, salt)`. Interactive prompt for the CLI;
  for the daemon the passphrase must arrive non-interactively
  (`ANIMUS_SECRET_PASSPHRASE` / credential) and is therefore equivalent in
  exposure to `user-key` (no human types it at runtime — documented).
- `device-id`: `HKDF(machine-id || per-install random salt)`. Binding only — the
  ids are readable, so this gives device-binding but not on-device secrecy.

Per-OS providers are `cfg`-gated so each compiles only where it applies; every
non-applicable target degrades to `device-id`/`user-key`.

## Config (protocol::Config)

- `secret_backend: auto | keyring | device | env` — `auto` keeps existing
  keyring installs on keyring (don't silently strand secrets), uses `device` for
  fresh installs / where keyring is absent.
- `secret_key_source: auto | user-key | passphrase | device-id`.
- `secret_key_file: <path>`.

`animus secret migrate` moves secrets between keyring and the device store
(idempotent, non-destructive by default). `env` always wins (unchanged).

## Crypto dependencies (vetted, no homegrown crypto)

`chacha20poly1305`, `argon2`, `hkdf` + `sha2`, `zeroize`, `rand` (OsRng).
Per-OS: `security-framework` (macOS), `windows` (DPAPI), `tss-esapi`/`systemd`
or machine-id read (Linux).
