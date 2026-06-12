# Security Reference

This page documents the security controls Animus applies at the install
boundary and the trust model behind plugin signature verification. For the
background design and threat model, see
[`docs/architecture/plugin-signing.md`](../architecture/plugin-signing.md).

## Plugin signature policy

`animus plugin install` verifies that every downloaded plugin binary was
signed by a trusted publisher before it is copied into
`~/.animus/plugins/`. Verification uses **cosign keyless** signatures
against a `<asset>.bundle` published next to the asset on the GitHub
Release. There is no PEM public key to manage: trust is anchored on the
signer identity (the Fulcio-issued cert SAN) plus the OIDC issuer
(`https://token.actions.githubusercontent.com`).

### Keyless trust model (v0.4.12)

Every `launchapp-dev/animus-*` release pipeline signs through GitHub
Actions OIDC + Sigstore Fulcio + the Rekor transparency log. There is
no static signing key; each signature uses a short-lived cert tied to
the workflow URI and tag. Animus verifies in three layers:

1. **Cryptographic validity** — cosign checks the signature against the
   per-signing Fulcio cert, walks the cert chain to the Sigstore Fulcio
   CA root (built into the cosign binary), and confirms the entry is
   present in the Rekor transparency log.
2. **Identity** — the cert SAN URI must match the trusted publisher's
   `identity_regex`. For `launchapp-dev`, that's
   `^https://github\.com/launchapp-dev/[^/]+/\.github/workflows/release\.yml@refs/tags/v.*`,
   which pins every release to the standardized `release.yml` workflow
   under a `v*` tag in any `launchapp-dev` repo.
3. **OIDC issuer** — the cert issuer must equal
   `https://token.actions.githubusercontent.com`, so a non-GitHub
   Actions signing path is rejected even if the SAN somehow matched.

No baked-in public key is involved. The pre-v0.4.12 key-based path
(`~/.animus/trusted-keys/<owner>.pem` + a baked
`LAUNCHAPP_DEV_COSIGN_PUBLIC_KEY_PEM` constant) was removed because the
real release pipeline never produced PEM-verifiable signatures.

### Install-time default: `warn`

The install-time default policy is `warn`. Verification still runs and
the resolved `signature_status` is recorded in `~/.animus/plugins.yaml`,
but a missing / invalid / untrusted-signer signature degrades to a
stderr warning rather than failing the install.

This default is intentional: it keeps the audit trail intact while
avoiding a hard install failure for plugins whose release pipeline does
not yet publish a keyless cosign bundle. Source of truth lives in
[`PolicyMode::default_for_install`](https://docs.rs/orchestrator-plugin-host/latest/orchestrator_plugin_host/signature_verifier/enum.PolicyMode.html#method.default_for_install)
and the CLI fallback in
`crates/orchestrator-cli/src/services/operations/ops_plugin.rs::effective_policy_mode`.

Under `warn`:

- Verification still runs and `signature_status` is still recorded in
  `~/.animus/plugins.yaml`, so the audit trail stays intact.
- Unsigned / invalid / untrusted-signer results log a warning to stderr
  and the install proceeds.

Operators who want fail-closed enforcement opt in per-install:

```bash
animus plugin install --signature-policy strict <owner>/<repo>
```

See [Recommended posture for production](#recommended-posture-for-production)
for the deployment-time configuration that pins every install to
`strict` without needing the flag on each invocation.

### Policy modes

`animus plugin install` exposes three enforcement modes via the
`--signature-policy <MODE>` flag. The default is
[`warn`](#install-time-default-warn).

| Mode       | Behavior                                                                                          | When to use                                                                |
| ---------- | ------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------- |
| `strict`   | Refuse install if the keyless signature is missing, invalid, or signed by an identity outside the trusted-publisher list. | Production environments — opt in per-install or via deployment config. |
| `warn`     | Verify when possible; log a warning to stderr and proceed on any failure. **DEFAULT.**            | Default for interactive use and environments where some plugins lack signatures. |
| `disabled` | Skip verification entirely.                                                                       | Air-gapped or local-build flows where signing is not feasible.             |

The legacy `--require-signature` and `--skip-signature` flags are
retained for backward compatibility. They map to `--signature-policy
strict` and `--signature-policy disabled` respectively.

`--allow-unsigned` is a convenience alias for `--signature-policy warn`.

#### Strict mode (opt-in)

```bash
animus plugin install --signature-policy strict launchapp-dev/animus-provider-claude
```

When strict mode rejects an install you'll see one of:

- `signature policy is strict but no cosign signature could be verified: <reason>` — no `.bundle` was published, or cosign isn't on `$PATH`.
- `cosign signature verification FAILED; refusing install: <message>` — the bundle exists but cosign rejected it cryptographically (bad signature, missing Rekor entry, expired cert, etc.).
- `signature is valid but the signer is not in trusted-signers.yaml` — the cert identity isn't on your allowlist (or no `TrustedPublisher` is configured for the owner).

Each error suggests the appropriate override flag (`--allow-unsigned`,
`--signature-policy disabled`, or adding the signer to
`~/.animus/trusted-signers.yaml`).

#### Warn mode

```bash
animus plugin install --signature-policy warn launchapp-dev/animus-subject-experimental
# or equivalently
animus plugin install --allow-unsigned launchapp-dev/animus-subject-experimental
```

Verification still runs and the result is recorded in
`~/.animus/plugins.yaml` under `signature_status`, but the install
proceeds even on failure. Use only when migrating publishers onto
cosign keyless signing.

#### Disabled mode

```bash
animus plugin install --path ./my-local-build.bin --signature-policy disabled
```

Skips verification entirely. Use this for locally built `--path` installs
or direct `--url --sha256` installs that have no upstream signature bundle.

### Trusted publishers (built-in)

The keyless model maps GitHub owners to per-publisher identity regex +
OIDC issuer combinations. Animus ships one built-in entry:

| Owner          | Identity regex                                                                                     | OIDC issuer                                          |
| -------------- | -------------------------------------------------------------------------------------------------- | ---------------------------------------------------- |
| `launchapp-dev`| `^https://github\.com/launchapp-dev/[^/]+/\.github/workflows/release\.yml@refs/tags/v.*`           | `https://token.actions.githubusercontent.com`        |

This is the trust anchor for every `launchapp-dev/animus-*` release.
The regex is anchored at the start (no prefix-attack surface), escapes
literal dots (`\.`), pins the standardized `release.yml` workflow, and
requires a `v*` tag — so only release-channel builds are trusted.

Additional publishers can be added programmatically via
`SignaturePolicy::trusted_publishers` (Rust API). A YAML-configurable
publisher list is tracked for v0.5.

**Operational caveat (v0.4.12):** the CLI install path applies the
keyless identity regex (`<owner>/<repo>` → `^https://github\.com/<owner>/<repo>/.+`)
on a *per-install-source* basis through `verify_with_cosign`, and the
optional `~/.animus/trusted-signers.yaml` glob allowlist is the only way
to narrow the trust set further. When `trusted-signers.yaml` is missing
or empty, any release whose cert chain validates and whose SAN matches
the per-install identity regex is accepted — pre-populate the file to
restrict trust to specific owners (e.g. `launchapp-dev/animus-*`). The
plugin-host `TrustedPublisher` list is the trust surface used by the
in-host `verify_plugin_install` entry point and by future MCP /
daemon-side installs.

### Manual cosign verification

To verify a published release artifact by hand:

```bash
REPO=animus-transport-graphql
VER=v0.2.3
cd /tmp && mkdir -p cosign-check && cd cosign-check
gh release download $VER --repo launchapp-dev/$REPO --pattern '*.tar.gz' --pattern '*.bundle'
for tar in *.tar.gz; do
  cosign verify-blob \
    --certificate-identity-regexp "https://github.com/launchapp-dev/$REPO/.github/workflows/release\.yml@refs/tags/v.*" \
    --certificate-oidc-issuer "https://token.actions.githubusercontent.com" \
    --bundle "$tar.bundle" \
    "$tar"
done
```

Cosign prints `Verified OK` for each artifact when the signature chain
holds. This is exactly the command Animus runs internally for keyless
verification — the only difference is that Animus pulls the identity
regex from the matching `TrustedPublisher` instead of requiring you to
hand-build it.

### Cosign binary dependency

Verification shells out to the `cosign` binary. If `cosign` is not on
`$PATH`:

- Under `strict`: the install fails with an actionable
  `install cosign from https://github.com/sigstore/cosign` error.
- Under `warn`: the install proceeds and `signature_status` is recorded
  as `unsigned` with the same reason.
- Under `disabled`: cosign is never consulted.

A migration to the in-process `sigstore` Rust crate is planned for
v0.5+. The CLI flag surface will not change.

### Signature status field

Every successful install records one of these stable strings under
`signature_status` in `~/.animus/plugins.yaml`:

| Value              | Meaning                                                                              |
| ------------------ | ------------------------------------------------------------------------------------ |
| `verified`         | cosign accepted the keyless bundle against a trusted publisher.                      |
| `unsigned`         | No bundle was published, no trusted publisher matched, or cosign wasn't installed.   |
| `invalid`          | A bundle was published but cosign rejected it (bad sig, expired cert, missing Rekor).|
| `untrusted_signer` | A bundle verified, but the cert identity isn't on the trusted-publisher list.        |
| `skipped`          | Verification was bypassed (`--signature-policy disabled` / `--skip-signature`).      |

`animus plugin list` surfaces this in the `SIG` column.

## Trusted orgs (TOFU)

Independent of cosign verification, every `animus plugin install` from a
release source also consults a separate allowlist at
`~/.animus/trusted-orgs.yaml`. Installing from an org not in this list
prompts the operator at the TTY (non-suppressible) or fails non-interactively.

Built-in trusted orgs: `launchapp-dev` (the trust anchor for the canonical
Animus plugins — it cannot be revoked).

See [`docs/architecture/plugin-signing.md`](../architecture/plugin-signing.md#trusted-orgs-tofu)
for the threat model.

### Trust lifecycle: trust → audit → revoke → re-trust

The TOFU store is **audited**. Each trust grant is persisted as a rich
record rather than a bare org name:

| Field          | Meaning                                                                        |
| -------------- | ------------------------------------------------------------------------------ |
| `org`          | GitHub owner/org slug.                                                          |
| `trusted_at`   | RFC3339 timestamp of when trust was first granted.                             |
| `decided_by`   | `interactive-prompt` (typed `yes`), `yes` (`--yes`/`--force`), `allow-org` (`--allow-org`), or `built-in`. |
| `first_plugin` | The `owner/repo` whose install first triggered the prompt.                     |
| `revoked_at`   | RFC3339 timestamp of revocation. Present only on tombstones (see below).        |

**Back-compat:** the loader still reads the legacy bare-string format
(`trusted_orgs: [- some-org]`). Legacy entries load as active records with
no timestamps; the next grant rewrites them in the rich shape.

1. **Trust.** On the first install from an unknown org, Animus prompts at
   the TTY (or fails non-interactively, directing you to `--allow-org` /
   `--yes`). Accepting writes a rich record with `trusted_at` +
   `decided_by` + `first_plugin`. Subsequent installs from the same org
   skip the prompt.

2. **Audit.** Inspect the store with:

   ```bash
   animus plugin trust list          # current + revoked, with timestamps
   animus plugin trust list --json
   ```

   Each successful release-source install also stamps an `org_trust`
   block (`org` + `trusted_at` + `decided_by`) into the install JSON
   envelope and the `plugin_install` audit line, so you can answer "when
   did we trust this org?" from the install record itself.

3. **Revoke.** Remove an org's trust:

   ```bash
   animus plugin revoke-trust evil-org
   ```

   This does **not** delete the record — it stamps a `revoked_at`
   tombstone. The tombstone keeps the audit trail intact and ensures the
   next install from that org re-prompts (a deleted record would silently
   re-trust on the next `--yes`). Revoking emits a `trust_org_revoked`
   audit event. The built-in `launchapp-dev` org cannot be revoked.

4. **Re-trust.** Installing from a tombstoned org re-runs the TOFU prompt.
   Accepting clears the `revoked_at` tombstone and stamps a fresh
   `trusted_at` / `decided_by`, so the record reflects the most recent
   decision while the prior revocation remains in the audit log stream.

> **v0.6 note.** TOFU remains warn-by-default in this release; the default
> does not flip to required-signature/strict enforcement until v0.6. TOFU
> is a convenience trust ledger, not a cryptographic trust anchor — cosign
> keyless verification (above) is the authenticity control.

## Manifest name vs repo basename

Install refuses any plugin whose `manifest.name` does not equal the repo
basename. This blocks typosquat attacks where an attacker publishes a
near-name release that shadows a legitimate plugin. `--force` overrides.

## Reserved provider tools

Provider plugins that claim one of the in-tree provider tools
(`claude`, `codex`, `gemini`, `opencode`, `oai-runner`) are refused by
default. `--allow-shadow-builtin` is required to deliberately shadow an
in-tree backend.

## Configuration files

| File                                  | Purpose                                                                          |
| ------------------------------------- | -------------------------------------------------------------------------------- |
| `~/.animus/trusted-signers.yaml`      | Optional glob allowlist for cosign cert identities. **Missing / empty = permissive** (any keyless signature whose cert chain validates is accepted, regardless of owner). Populate this file to scope the trust set down. |
| `~/.animus/trusted-orgs.yaml`         | TOFU allowlist of GitHub orgs the operator has accepted (orthogonal to cosign trust). Rich per-entry records carry `trusted_at` / `decided_by` / `first_plugin`, plus `revoked_at` tombstones. Legacy bare-string entries still load. See [Trust lifecycle](#trust-lifecycle-trust--audit--revoke--re-trust). |
| `~/.animus/plugins.yaml`              | Installed-plugin registry. Records `signature_status` per entry.                 |
| `.animus/plugins.lock` (project) or `~/.animus/plugins.lock` (global) | Append-only integrity ledger pinning `sha256(artifact)` + `sha256(signature_bundle)` for every installed plugin. Project-local takes precedence when `<project_root>/.animus/` exists. |

### Lockfile fail-closed policy

`animus plugin install` and `animus plugin install-defaults` **refuse**
when the resolved `plugins.lock` exists but cannot be parsed or carries
an incompatible `schema_version`. The error surfaces (a) the exact
lockfile path, (b) the underlying loader error chain, and (c) two
remediation paths. This is intentional: an unreadable lockfile is an
audit-boundary event and silently overwriting it would erase the
recorded `sha256` history that `verify_installed` uses to detect
tamper on subsequent upgrades.

Remediation paths:

1. **Restore from version control or backup.** Project-local lockfiles
   live at `<project_root>/.animus/plugins.lock` and are intended to be
   checked in. `git checkout HEAD -- .animus/plugins.lock` is usually
   the right call.
2. **Re-run with `--force-rewrite-lockfile`.** This discards the
   unreadable file and rebuilds a fresh lockfile from this install
   onward. The install pipeline emits a `warn!` at this point that
   notes integrity history was reset; treat the warning as evidence
   that subsequent `--force` installs cannot detect pre-existing
   tamper until the new lockfile has accumulated enough entries.

The `--force-rewrite-lockfile` flag is **CLI-only**: MCP and control-plane
install routes default to fail-closed with no override, on the
principle that lockfile recovery is an operator decision that should be
explicit and synchronous.

`~/.animus/trusted-keys/` is no longer consulted as of v0.4.12 — the
key-based PEM path it served is gone. Existing directories can be
deleted; Animus does not write to it.

## Recommended posture for production

1. Pass `--signature-policy strict` explicitly on every install. The
   library default ([`PolicyMode::default_for_install`]) is `warn` to
   keep interactive onboarding unblocked; production callers should
   override per-install (or wrap `animus plugin install` in a script
   that always supplies the flag).
2. Install `cosign` on every machine that runs `animus plugin install`.
   Strict mode fails closed without it.
3. Pre-populate `~/.animus/trusted-orgs.yaml` so non-interactive installs
   never block on a TOFU prompt.
4. Audit `signature_status` in `~/.animus/plugins.yaml` periodically.
   Anything other than `verified` or `skipped` for a `--path`/`--url`
   install is a policy violation worth investigating. `unsigned` rows
   under the default `warn` policy are the expected signal that the
   release pipeline for that plugin has not yet adopted keyless cosign
   bundles; treat them as a reminder to either contact the maintainer
   or reinstall under `--signature-policy strict` after confirming a
   bundle now ships.

[`PolicyMode::default_for_install`]: https://docs.rs/orchestrator-plugin-host/latest/orchestrator_plugin_host/signature_verifier/enum.PolicyMode.html#method.default_for_install

## Kill switches

One environment variable disables a plugin-system feature at daemon start
for emergency rollback:

- `ANIMUS_DAEMON_DISABLE_TRIGGERS=1` — skip the trigger plugin supervisor.

Requires a daemon restart to take effect and to re-enable. See
[`docs/reference/configuration.md`](configuration.md#plugin-kill-switches).

`ANIMUS_PROVIDER_DISABLE_PLUGIN` was removed in v0.4.12 when the in-tree
provider backends were extracted to standalone plugins. There is no longer
a runtime escape hatch for provider dispatch — uninstall or disable the
offending plugin instead.

## Multi-tenant + RBAC roadmap

Animus ships a v0.5.8 small-core RBAC slice. The control socket is still
local-OS-user scoped, but the control path now understands
`~/.animus/principals.yaml`, `policy.rbac` (`single-user` default or
`enforce`), `animus auth whoami`, and the honor-system `--as <principal>`
override that is logged loudly and rejected under `enforce` when peer
credentials do not permit the impersonation.

The v0.5.5+ design proposal at
[`docs/architecture/multi-tenant-rbac-v0.5.5.md`](../architecture/multi-tenant-rbac-v0.5.5.md)
lays out a typed `Principal` model and a four-chokepoint RBAC scaffold
(control dispatch, plugin install, secret read, audit write). v0.5.8
implements chokepoint #1 only: control dispatch now carries the
per-request `Permission` constant that gates RPCs such as
`workflow.run`, `queue.mutate`, `subject.update`, `plugin.uninstall`,
and `daemon.shutdown`. The remaining chokepoints, per-tenant state
isolation, and per-principal secret routing are still deferred to v0.6.
