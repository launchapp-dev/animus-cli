# Self-update — `animus update` / `animus self update`

This page documents the binary self-update path used by both
`animus update` (v0.5.8 top-level alias) and `animus self update`
(the original, kept for back-compat).

## Source of truth

- Releases are fetched from **`launchapp-dev/animus-cli`** GitHub
  Releases (the public mirror of this repo).
- The CLI calls `GET https://api.github.com/repos/launchapp-dev/animus-cli/releases/latest`
  for `--channel stable`, or `GET .../releases` (listing) for
  `--channel nightly`.
- User-Agent is `animus-update/<CARGO_PKG_VERSION>` on every request.

## Asset naming

The release workflow (`.github/workflows/release.yml`) publishes one
archive per supported (os, arch) target. As of v0.5.8 the live mapping
is:

| Host | Target triple | Asset name |
|---|---|---|
| macOS, Apple Silicon | `aarch64-apple-darwin` | `ao-vX.Y.Z-aarch64-apple-darwin.tar.gz` |
| macOS, Intel | `x86_64-apple-darwin` | `ao-vX.Y.Z-x86_64-apple-darwin.tar.gz` |
| Linux, x86_64 | `x86_64-unknown-linux-gnu` | `ao-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz` |
| Windows, x86_64 | `x86_64-pc-windows-msvc` | `ao-vX.Y.Z-x86_64-pc-windows-msvc.zip` |

Notes:

- Linux `aarch64` is not currently published. `animus update` will exit
  with `no release asset matched platform ...` if invoked on that host.
- Windows `.zip` archives are published but `animus update`'s in-tree
  extractor handles `.tar.gz` / `.tgz` only. Windows users should
  download the zip from the Releases page and replace the binary
  manually until a `.zip` extractor lands.
- `apply_update` falls through to a bare-binary install when the asset
  has no archive extension — `looks_like_installable_asset` is the
  allowlist.

Asset selection is handled by `pick_asset_for_host` in
`crates/orchestrator-cli/src/services/self_update.rs`. It accepts
whole-token matches against the os and arch components and recognises
common aliases (`darwin` for `macos`, `arm64` for `aarch64`, `amd64`
for `x86_64`).

## Verification posture

- The release asset metadata carries a `digest` field of the form
  `sha256:<hex>` per GitHub Release API. When present, the staged
  download is hashed via [`sha2::Sha256`] and the result is compared
  byte-for-byte. Mismatch aborts the install with a clear error.
- When the inline `digest` is absent, the installer looks for a sibling
  `<asset>.sha256` asset in the same release (literal match; the
  `.sha256sum` extension is NOT searched — the `find_sidecar`
  implementation only constructs `format!("{}.sha256", asset.name)`).
  If found, the leading hex token is parsed and compared.
- The release workflow currently publishes a single aggregate
  `SHA256SUMS.txt` rather than per-asset sidecars. The inline `digest`
  on the asset metadata is the primary verification path.
- **Cosign keyless signatures are not currently produced** by the
  release workflow. There is no `*.bundle` artifact to verify. If
  `cosign` lands on the release pipeline in a later iteration,
  `apply_update` should add a verify pass alongside the existing
  sha256 check — soft-fail (warn + continue) when `cosign` is not on
  the local `PATH`, hard-fail when verification itself fails.

## Network posture

The HTTP client in `build_client` enforces three defences:

1. A short per-request timeout (`STARTUP_FETCH_TIMEOUT` /
   `MANUAL_FETCH_TIMEOUT`) so a hung GitHub call never delays a
   subcommand by more than a few seconds.
2. A redirect cap of 5 hops (`MAX_REDIRECTS`). The release-download
   path is one or two hops in practice.
3. A host allowlist via `is_allowed_release_host` — every redirect
   target must resolve to `api.github.com`, `github.com`,
   `codeload.github.com`, or any `*.githubusercontent.com` subdomain.
   A redirect to any other host fails the request before bytes are
   read.

## Atomic swap

`atomic_install` is the only file-level mutator. It:

1. Resolves the running binary via `std::env::current_exe()` —
   stable on Linux + macOS for `/usr/local/bin/animus`-style installs.
2. Writes the new binary to `<current_exe>.new` *in the same
   directory* (POSIX requires `rename(2)` to be atomic only within
   the same filesystem; cross-mount renames fail with `EXDEV`).
3. Sets executable mode (`0o755` on Unix) via `set_permissions`.
4. Calls `std::fs::rename(<new>, <current_exe>)`.

`rename(2)` on POSIX is atomic: the running process keeps its open
file descriptor pointing at the old inode, but every subsequent
`exec` of the path picks up the new bits. This is the only safe shape
— writing directly to `<current_exe>` would corrupt a running daemon,
and staging in `/tmp` then `mv`-ing would fail with `EXDEV` whenever
`/tmp` lives on a separate filesystem.

## Rollback

There is no built-in rollback. To revert manually:

```bash
# 1. Find the version you want to revert to:
gh release list --repo launchapp-dev/animus-cli

# 2. Download + extract the archive for your host:
TAG=v0.5.5
TARGET=aarch64-apple-darwin
curl -fL \
  -o /tmp/ao-${TAG}.tar.gz \
  https://github.com/launchapp-dev/animus-cli/releases/download/${TAG}/ao-${TAG}-${TARGET}.tar.gz
tar -xzf /tmp/ao-${TAG}.tar.gz -C /tmp/

# 3. Replace the running binary at its current path:
INSTALL_PATH=$(which animus)
sudo install -m 0755 "/tmp/ao-${TAG}-${TARGET}/animus" "${INSTALL_PATH}"

# 4. Restart any running daemons:
animus daemon stop
animus daemon start
```

The same-directory rename in step 3 keeps any running `animus daemon`
process pointed at the old inode until restart, so the rollback is
race-free.

## Channels

| `--channel` | `AutoUpdateChannel` | Behaviour |
|---|---|---|
| `stable` (default) | `Stable` | Calls `GET /releases/latest`, accepts only `prerelease: false` records. |
| `nightly` | `Prerelease` | Calls `GET /releases` (listing), accepts any newer-than-current release including prereleases. |

There is no separate `*-nightly` tag stream today — `nightly` maps
to the GitHub prerelease admission rule. If a dedicated nightly
publishing cadence ships later, the mapping can be tightened without
changing the CLI surface.

## Environment overrides

These act on the background startup check (when the daemon spawns
`animus` for routine work), not on the manual `animus update` command:

- `ANIMUS_AUTO_UPDATE_DISABLE=1` — skip the startup check entirely.
- `ANIMUS_AUTO_UPDATE_MODE={off,notify,prompt,auto}` — override the
  configured mode in the project-local `.animus/config.json` (read
  first by `resolve_effective_config_block`) or the global
  `~/.animus/config.json` (read second; resolved via
  `Config::global_config_dir()`). The project-local block wins when
  both are present.

Manual `animus update` invocations always run regardless of these
env vars — the operator is asking for it explicitly.
