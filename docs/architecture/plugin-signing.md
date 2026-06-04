# Plugin Release Signing

## Status

Implemented in animus-cli v0.4.x and plugin release.yml v0.1.2+. Verification
shells out to the `cosign` binary; the design assumed Rust-native
`sigstore-rs` (still planned for v0.5+ — the user-facing CLI surface won't change).

## Why

Today, `animus plugin install launchapp-dev/<repo>` downloads a binary and verifies a SHA256 sidecar. That guarantees **integrity** (the binary wasn't corrupted in transit), but not **authenticity** — we still implicitly trust GitHub's TLS plus the credentials on the `launchapp-dev` account. If a release token leaks, or a maintainer's account is compromised, a malicious binary can ship under the right name and the right SHA256, and Animus has no way to tell.

For v0.2 we want cryptographic signing so that:

- A plugin from `launchapp-dev/animus-provider-claude` is provably built by the Launchapp.dev release pipeline, not someone with stolen GitHub credentials.
- The Animus CLI can refuse to install an unsigned plugin (opt-in strict mode) or warn (default).
- Plugin authors can independently sign their plugins, and users can pin trust to specific signers without going through Launchapp.

## Goals

- **Authenticity.** Prove a plugin was built by the claimed publisher.
- **Reproducibility of verification.** Signatures bind to a specific binary (by digest), not to a moving tag or download URL.
- **Decentralization.** Any plugin publisher can sign their own releases. Launchapp does not gatekeep.
- **Opt-in strictness.** Users choose between "verify when present", "require signed", or "ignore".
- **Low-friction publishing.** Signing happens automatically inside `release.yml` with zero secret management on the publisher side.

## Non-goals

- Replacing TLS/HTTPS for transport security.
- A revocation system. v0.2 leaves this to "rotate the tag and ship a new release"; a proper revocation flow can come later.
- Proving the **source code** matches the binary. That is reproducible builds, and it is a separate concern.

## Approach: sigstore + cosign keyless signing

We adopt the [sigstore](https://www.sigstore.dev) ecosystem and use [cosign](https://docs.sigstore.dev/cosign/signing/overview/) in its **keyless** mode.

Keyless signing works like this:

1. The GitHub Actions runner requests an OIDC token from GitHub's OIDC issuer (`https://token.actions.githubusercontent.com`). The token's identity claims include the repository, the workflow file, and the ref that triggered the run.
2. Cosign generates an **ephemeral** keypair on the runner. It sends the public key plus the OIDC token to sigstore's Fulcio CA, which issues a short-lived X.509 code-signing certificate that embeds the OIDC identity (e.g. `https://github.com/launchapp-dev/animus-provider-claude/.github/workflows/release.yml@refs/tags/v0.2.0`).
3. Cosign signs the binary's digest with the ephemeral private key, then discards the key.
4. The signature, certificate chain, and a Rekor transparency-log inclusion proof are packaged into a single `.bundle` file.

The result: there are no long-lived signing keys to manage, leak, or rotate. The identity that signed each release is publicly auditable via Rekor, and verifiers can require that the identity match a specific `<owner>/<repo>/<workflow>` pattern.

## Release pipeline changes per plugin repo

Each plugin repo's `.github/workflows/release.yml` gains two steps after the binary build matrix:

```yaml
- uses: sigstore/cosign-installer@v3
- name: Sign binary with cosign keyless
  run: |
    cosign sign-blob \
      --yes \
      --bundle animus-provider-claude-${{ matrix.target }}.bundle \
      animus-provider-claude-${{ matrix.target }}
- name: Upload signature bundle
  uses: actions/upload-release-asset@v1
  with:
    asset_path: animus-provider-claude-${{ matrix.target }}.bundle
    asset_name: animus-provider-claude-${{ matrix.target }}.bundle
```

The workflow needs `id-token: write` permission so the runner can mint the OIDC token. One `.bundle` is produced per binary asset and uploaded next to the binary on the GitHub Release.

## CLI changes

`animus plugin install` learns three flags:

```bash
animus plugin install launchapp-dev/animus-provider-claude
# default: verify the signature if a .bundle is present; warn if absent.

animus plugin install --require-signature launchapp-dev/animus-provider-claude
# strict: refuse to install if the bundle is missing or verification fails.

animus plugin install --skip-signature launchapp-dev/animus-provider-claude
# escape hatch for plugins that haven't adopted signing yet.
```

The install output JSON envelope (`animus.cli.v1`) and `animus plugin list` both gain a field:

```
signature_status: verified | unsigned | invalid
```

`verified` includes the certificate identity (`<owner>/<repo>/.github/workflows/<file>`) and the Rekor log index, so users can audit which workflow actually signed the binary they're running.

## Verification logic

When verifying a downloaded binary:

1. Look for `<asset>.bundle` next to the asset in the GitHub Release.
2. If present, perform the equivalent of:
   ```
   cosign verify-blob \
     --bundle <asset>.bundle \
     --certificate-identity-regexp 'https://github.com/<owner>/<repo>/.*' \
     --certificate-oidc-issuer 'https://token.actions.githubusercontent.com' \
     <asset>
   ```
3. Confirm that the certificate's identity claim resolves to the same `<owner>/<repo>` that the user typed into `animus plugin install`. Reject if it was issued for a different identity.
4. Cache the verification result keyed by `(asset_sha256, identity)` so re-installs don't re-hit Rekor.

Implementation choice: prefer the [`sigstore`](https://crates.io/crates/sigstore) Rust crate so we can verify in-process without depending on the `cosign` binary at runtime. Shelling out to `cosign` is acceptable as a stopgap if the Rust crate is missing a feature we need; the public CLI surface stays the same either way.

## Trust model

The default identity check is: **the binary was signed by a workflow in the GitHub repo I downloaded it from.** That's enough to defeat the basic "stolen account ships a malicious binary" scenario, as long as the attacker can't also push a malicious `release.yml` to the same repo.

Users can extend the trust set with an explicit allowlist at `~/.animus/trusted-signers.yaml`:

```yaml
trusted_signers:
  - identity: "launchapp-dev/animus-*"
    issuer: "https://token.actions.githubusercontent.com"
  - identity: "alice/my-private-plugin"
    issuer: "https://token.actions.githubusercontent.com"
```

Patterns are matched as globs against `<owner>/<repo>`. When `--require-signature` is set and the install target doesn't match a trusted signer, the CLI refuses regardless of whether the bundle itself verifies. This lets organizations pin Animus installations to a known publisher set.

## Migration path

- **v0.4.x (animus-cli) + v0.1.2 (plugin repos) — CURRENT.** Signing is ENABLED in the release workflows of all `launchapp-dev/*` plugins. Tags published from v0.1.2 onward ship signed binaries (`<asset>.tar.gz.bundle` alongside each asset). The animus-cli verifies by default when a bundle is present and warns/installs when one is absent. Verification shells out to the `cosign` binary; when `cosign` isn't on `$PATH`, installs proceed and the registry records `signature_status: unsigned`. Use `--require-signature` to refuse installs that don't verify, `--skip-signature` as an escape hatch.
- **v0.5.x.** Switch CLI verification from shell-out to in-process via the `sigstore` Rust crate. CLI flag surface stays stable.
- **v0.6.x.** Default flips to `--require-signature` for installs from `launchapp-dev/*`. Third-party plugins still install with a warning. The escape hatch is `--skip-signature`.
- **v1.0+.** Unsigned installs are deprecated entirely. `--skip-signature` survives as an explicit opt-out for air-gapped or local-build workflows; everything else must be signed.

## Implementation notes (v0.4.x)

- The trusted-signers config lives at `~/.animus/trusted-signers.yaml` (overridable via `--trusted-signers <PATH>` or `$ANIMUS_TRUSTED_SIGNERS`). When the file is absent, the default is "verify any signer against the cert's stated identity for this repo" — i.e. no allowlist enforcement.
- The install pipeline records `signature_status` (one of `verified`, `unsigned`, `invalid`, `untrusted_signer`, `skipped`) in `~/.animus/plugins.yaml`. `animus plugin list` surfaces it in the `SIG` column.
- Strict-mode failures (`Invalid` and `UntrustedSigner` outcomes) abort install before the binary is copied into `~/.animus/plugins/`. `Unsigned` only aborts when `--require-signature` is set.
- The `cosign` binary requirement is intentionally soft: missing-cosign degrades to `signature_status: unsigned` rather than blocking installs.

## Stopgap supply-chain defenses (v0.4.x)

Until cosign verification is required-by-default in v0.6, three additional
checks guard the install boundary. They are loud-by-default and override-able
for legitimate edge cases.

### Manifest name vs repo basename

`animus plugin install evil-org/animus-provider-claude` fetches the release
asset, probes `--manifest`, and refuses installs where `manifest.name` does
not equal the repo's basename (`animus-provider-claude` in the example). The
mismatch is the most common shape of supply-chain typosquats — an attacker
who can publish a release under a near-name needs to either also rename the
binary (which breaks every downstream tool that resolves by manifest.name) or
get caught by this check. `--force` overrides the check and emits a
`tracing::warn!` for the audit log.

### Reserved provider tools

A provider plugin whose `provider_tool` resolves to one of the reserved
first-party provider names (`claude`, `codex`, `gemini`, `opencode`, `oai`, or
`oai-runner`) would shadow the expected provider route. Install refuses these
plugins by default; pass `--allow-shadow-builtin` to opt in intentionally. The
install-time gate is the primary defense now that there is no in-tree provider
backend left to warn about at runtime.

The reserved list lives in
`crates/orchestrator-plugin-host/src/session/session_backend_resolver.rs` as
`RESERVED_PROVIDER_TOOLS`.

### Trusted orgs (TOFU)

A separate allowlist at `~/.animus/trusted-orgs.yaml` (override with
`$ANIMUS_TRUSTED_ORGS`) records which GitHub owners the operator has trusted
for plugin installs. Built-in trust: `launchapp-dev` (the canonical Animus
plugins).

```yaml
trusted_orgs:
  - launchapp-dev
  - my-internal-org
```

Installing from an owner that's not in this list prints a non-suppressible
warning and prompts the operator at the TTY:

```
warning: you are installing a plugin from `evil-org`, which is not a trusted
organization. Verify this is the intended publisher before continuing. Type
'yes' to trust this org for future installs, anything else to abort.
```

Override flags:

- `--allow-org <OWNER>` (repeatable): pre-trust additional owners for this
  install. Persists into `trusted-orgs.yaml` after the install succeeds.
- `--yes`: auto-confirm the TOFU prompt non-interactively.
- `--force`: also bypasses the prompt (subsumes `--yes` for this check).

On every successful install from a release source, the installing owner is
written to `trusted-orgs.yaml` so a follow-up install of another plugin from
the same publisher skips the prompt. This is *trust on first use*, not a
trust anchor: it does not change anything about cosign verification, and it
will be subsumed by required-signature mode in v0.6.

## Open questions

- Should `trusted-signers.yaml` ship pre-populated with `launchapp-dev/animus-*` so that fresh installs get a safe default? Pro: stronger out-of-the-box guarantees. Con: makes Launchapp the de facto trust root, which contradicts the decentralization goal.
- Should we sign the `animus-plugin-registry/plugins.json` index too, or is HTTPS plus a checksum in the CLI release sufficient? Signing the index would let users verify "this is the same registry the CLI was built against" but adds another moving piece.
- How do we handle plugins distributed outside GitHub Releases — private artifact registries, S3 buckets, on-prem mirrors? Cosign supports OCI registries and arbitrary blob stores; the CLI's fetch layer needs an abstraction so the bundle URL is resolved alongside the binary URL, regardless of host.

## References

- Sigstore project — https://www.sigstore.dev
- Cosign signing overview — https://docs.sigstore.dev/cosign/signing/overview/
- `sigstore` Rust crate — https://crates.io/crates/sigstore
- npm provenance attestations (same model, different ecosystem) — https://docs.npmjs.com/generating-provenance-statements
