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
- The factories `build_secret_store(repo_scope, scoped_root)` and
  `build_secret_store_for_project(repo_scope, scoped_root, project_root)` read
  config and return the configured backend. Project-aware call sites use the
  latter so the global and project `secrets` blocks are merged before backend
  and key-source selection. They replace the ~5 direct
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

Selected by config `secrets.key_source`:

- `auto` (default): resolves in priority order — (1) `ANIMUS_SECRET_KEY` env var
  → `user-key`; (2) `key_file` configured in the `secrets` block → `user-key`;
  (3) `ANIMUS_SECRET_PASSPHRASE` env var → `passphrase`; (4) `device-id` fallback
  (interactive hosts). Steps 1–3 make headless/server deployments work without
  setting `key_source` explicitly: supplying the key via env or file is enough, and
  `auto` selects the right source automatically. This avoids the
  keyring-unavailable hard error and prevents the device-id redeploy wipe caused by
  a new machine-id on container rebuild. The hardware key sources (Secure Enclave /
  DPAPI / TPM) are deferred — see "Platform support" below.
- `user-key`: operator-supplied 32-byte key from `ANIMUS_SECRET_KEY`
  (hex or base64) or a `key_file` path. For headless/server with a deploy-injected
  key (systemd `LoadCredential`, mounted secret, external KMS).
- `passphrase`: `Argon2id(passphrase, salt)`. The passphrase arrives via
  `ANIMUS_SECRET_PASSPHRASE` for both the CLI and the daemon — env-driven and
  script-safe, with no TTY-only path that would break under automation. In
  exposure terms it is therefore equivalent to `user-key` (no human types it at
  runtime). Resolution errors with the variable name when it is unset.
- `device-id`: `HKDF(machine-id || per-install random salt)`. Binding only — the
  ids are readable, so this gives device-binding but not on-device secrecy.

### Platform support (what's implemented)

`device-id` is the implemented cross-platform default and delivers the no-prompt,
device-bound behavior on **macOS** (IOPlatformUUID) and **Linux**
(`/etc/machine-id`). The hardware key sources are deliberately *not* shipped in
this revision, each for a concrete reason — `device-id`/`user-key`/`passphrase`
already cover every target:

- **macOS Secure Enclave** — deferred. Reaching an SE key goes through the
  keychain/SecItem APIs, which re-introduce exactly the unstable-signature
  re-prompt this feature exists to avoid. `device-id` is the better default here;
  an SE upgrade only pays off paired with stable code signing.
- **Windows DPAPI** — future work. It is the clean Windows hardware path, but it
  is a stateful protected-blob shape (not a deterministic key) and cannot be
  compiled or tested from the current dev environment. Until it lands, Windows
  uses `user-key` / `passphrase` (`auto` errors with that guidance, since the
  device-id machine-GUID read is not yet wired there).
- **Linux TPM** — out of scope under the repo's rust-only dependency policy: the
  practical TPM stack (`tss-esapi`) links the TSS2 **C** library. `device-id`
  (machine-id) is the Linux path.

## Config (protocol::Config)

The settings live in the `secrets` object in global or project `config.json`:

- `secrets.backend: auto | keyring | device | env` — `auto` keeps existing
  keyring installs on keyring (don't silently strand secrets) and selects
  `device` when server key material is configured.
- `secrets.key_source: auto | user-key | passphrase | device-id`.
- `secrets.key_file: <path>`.

`animus secret migrate` moves secrets between keyring and the device store
(idempotent, non-destructive by default). `env` always wins (unchanged).

## Crypto dependencies (vetted, no homegrown crypto)

`chacha20poly1305`, `argon2`, `hkdf` + `sha2`, `zeroize`, `rand` (OsRng) —
all pure-Rust RustCrypto, satisfying the repo's rust-only dependency policy.
The shipped key sources need no per-OS native crate: `device-id` reads the
machine id directly (`/etc/machine-id` on Linux, `ioreg` IOPlatformUUID on
macOS). The hardware sources that would pull `security-framework` /
`windows` / `tss-esapi` are deferred (see "Platform support").
