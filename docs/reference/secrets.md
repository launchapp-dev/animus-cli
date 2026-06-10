# Secrets

v0.5.8 adds project-scoped secret storage backed by the OS keychain. Plugin credentials (API tokens, OAuth refresh tokens, anything you'd otherwise dump into a `.env` file) live in macOS Keychain / libsecret / Windows Credential Manager rather than in plaintext on disk.

## Why

Before v0.5.8, plugin credentials reached the daemon through two patterns, both bad:

1. `~/.zshrc` exports — leaks via dotfile backup, shell history, `ps -e`, IDE auto-share.
2. A `.env` file at the project root — leaks via accidental commit, shoulder-surf, file-sync tools.

The keychain is local, OS-protected, and accessed through a small CLI surface. Plugin code keeps reading `std::env::var("LINEAR_API_TOKEN")` — no plugin-side changes needed.

## CLI surface

```sh
animus secret set <KEY> [--value <VAL>]   # value from --value or stdin
animus secret get <KEY>                    # print value to stdout
animus secret list                         # KEYs only — never values
animus secret rm <KEY>                     # remove from keychain
animus secret import-env [--file PATH]     # bulk migrate from .env
animus secret export-env [--file PATH]     # bulk export to .env (loud warn)
```

All commands honor the global `--project-root` flag and `--json` for machine-readable output on the `animus.cli.v1` envelope. The `auth` and `secret` surfaces also honor the global `--as <PRINCIPAL>` override so audit records can attribute a mutation to the declared principal when the local operator is acting on their behalf.

### Pipe-safe `set`

```sh
echo -n "$TOKEN" | animus secret set LINEAR_API_TOKEN
# or
pbpaste | animus secret set OPENAI_API_KEY
```

When `--value` is omitted, `set` reads stdin until EOF and strips a trailing `\n` / `\r`.

### `get` is shaped for piping

The plain (non-JSON) mode prints just the value to stdout, so you can pipe it into other tools. If stdout is a TTY a warning is emitted to stderr; pass `| pbcopy` / `| xclip` to avoid leaving values in shell scrollback.

### `list` and `index.json`

The keychain itself doesn't support listing entries by service prefix portably. Animus tracks the set of KEYs that belong to each project in a tiny JSON index at:

```
~/.animus/<repo-scope>/secrets/index.json
```

The index stores KEY names only — every actual value lives in the OS keychain. The file is safe to back up or commit-to-private-storage if you want to track which secrets a project needs without leaking values.

## Storage layout

- **Service name:** `animus:<repo-scope>` (e.g. `animus:auth-main-5ba84d1bbafc`). Two projects with the same KEY do not collide because the `repo-scope` segment differs.
- **Account name:** the KEY itself (`LINEAR_API_TOKEN`, `OPENAI_API_KEY`, …).
- **Index:** `~/.animus/<repo-scope>/secrets/index.json` — see above.

The `repo-scope` is the same value used everywhere else under `~/.animus/<repo-scope>/`; see `protocol::repository_scope::repository_scope_for_path`.

## Precedence rules

When a plugin asks for `LINEAR_API_TOKEN` at spawn time:

1. **Explicit process env wins.** If the daemon was started with `LINEAR_API_TOKEN=foo animus daemon start`, the plugin sees `foo` even when a different value sits in the keychain. This matches the documented contract so CI overrides keep working.
2. **Keychain entries are next, but only for variables the plugin's manifest declared.** Any KEY listed in the manifest's `env_required` block is read from the keychain (when set) and merged into the spawned plugin's environment. Entries that aren't declared in the manifest are **not** injected — this preserves the v0.4.x env trust boundary so a plugin can't accidentally read a credential it didn't ask for.
3. **Otherwise unset.** The plugin's manifest declares the var as `required = true`; the host emits a startup warning if neither source has it set.

Workflow YAML `${VAR}` interpolation follows the same chain: `std::env` first, then the keychain, then the `${VAR:-default}` fallback. `${VAR:?error}` shapes still fail with file path + line number when neither source has the value.

When a keychain-resolved `${VAR}` value (or a `${secret.<name>}` value) would be echoed back in a YAML parse diagnostic, the compiler replaces it with `[redacted:<name>]`. Values resolved from the explicit process environment are not redacted. References inside YAML comments are never interpolated, so a commented-out `# export ${LINEAR_TOKEN}` line cannot fail the compile or pull a value out of the keychain.

## Migration: `.env` → keychain

```sh
animus secret import-env --file .env
```

By default, KEYs that already have a value in the keychain are skipped; pass `--overwrite` to replace them. After importing, double-check with `animus secret list` and delete the source `.env` (or `.gitignore` it). The brief is: **the keychain is the source of truth; .env is migration shrapnel.**

## Threat model

What an attacker who reads your home directory CAN do:

- Read the per-scope index file (`~/.animus/<repo-scope>/secrets/index.json`) — sees KEY *names* but no values.
- See the broader scoped state under `~/.animus/<repo-scope>/`.

What they CANNOT do without an unlocked keychain session:

- Read the actual secret values. On macOS the Keychain prompts on first access by a new binary; on Linux libsecret demands the user's session bus.
- Bypass the per-scope index — entries written by other Animus projects (`animus:other-scope-...`) are inaccessible to this binary's account / service tuple.

What the design does NOT defend against:

- A root-equivalent attacker on the machine. If the local user can read keychain items, so can `cat /etc/sudo`.
- Phishing of the user's keychain prompt.
- Plaintext leakage from `animus secret get` to a logged-in TTY. The tool warns, but if you run it inside `tee /tmp/foo`, that's on you.

## Headless and CI

CI containers typically have no D-Bus session and no macOS Keychain. The recommended pattern is to keep using process env on the CI box:

```sh
LINEAR_API_TOKEN=$LINEAR_API_TOKEN \
OPENAI_API_KEY=$OPENAI_API_KEY \
animus daemon start
```

The keychain layer is a no-op in this configuration — the index file is empty, the spawn-path merge is empty, and behavior is byte-identical to pre-v0.5.8.

## Audit log

Every mutation (`set`, `rm`, `import-env`, `export-env`) writes a line to the per-scope audit log under `~/.animus/<repo-scope>/audit.jsonl`. Only KEY names land in the log — never values. The actor is the resolved Principal (see [RBAC]). Example record:

```json
{
  "ts": "2026-06-07T18:14:21Z",
  "actor": "user",
  "event": "policy_override",
  "details": {"event": "secret_set", "key": "LINEAR_API_TOKEN"},
  "principal": {"id": "sami", "kind": "user"}
}
```

## Spawn-path injection cap

The plugin host enforces a 1 MiB cumulative cap (sum of `KEY.len() + VALUE.len()`) on what it merges from the keychain into a child plugin's env. Entries past the cap are skipped with a `tracing::warn!` line. The cap exists so a runaway keychain index never blows past the platform's `ARG_MAX`.

## Workflow-runner subprocess allowlist

The daemon-spawned workflow runner is a regular binary (not a plugin), so it has no manifest the host can consult for env filtering. To keep the trust boundary tight, the daemon merges keychain entries into the runner's env only when the KEY matches a small fixed allowlist:

```
ANTHROPIC_API_KEY, OPENAI_API_KEY, GEMINI_API_KEY, GOOGLE_API_KEY,
GOOGLE_APPLICATION_CREDENTIALS, LINEAR_API_TOKEN, GITHUB_TOKEN, GH_TOKEN,
SLACK_BOT_TOKEN, SLACK_SIGNING_SECRET
```

Operators can extend this set per-deployment by exporting a comma-separated list in `ANIMUS_RUNNER_SECRET_ALLOWLIST`:

```sh
ANIMUS_RUNNER_SECRET_ALLOWLIST=ACME_TOKEN,VENDOR_X_KEY animus daemon start
```

Daemon-managed env keys (`ANIMUS_AGENT_RUN_ID`, the reattach socket path, the event pipe path) are always reserved and cannot be overridden from the keychain.

## Out of scope for v0.5.8

The following are non-goals and will land (or not) in v0.6+:

- Encrypted file backends (age, sops, similar) — only OS keychain for v0.5.8.
- External secret brokers (1Password / HashiCorp Vault / Doppler) — defer until the plugin role for "secret broker" is designed.
- Cross-machine sync — the keychain is local-only by design.
- Headless / Linux-no-D-Bus mode — for now CI uses process env directly.
- TUI for managing secrets.

[RBAC]: ./security.md
