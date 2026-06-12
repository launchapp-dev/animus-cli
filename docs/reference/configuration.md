# Configuration Reference

Animus resolves behavior from project YAML, installed pack layers, scoped runtime state, and environment overrides.

## Project-Local Sources

### `.animus/config.json`

Repository-local Animus configuration created during setup.

The optional `auto_update` block configures self-update behavior for the
`animus` CLI binary (see [Self-update](#self-update) below). Omitted by
default; absence behaves the same as `{"mode": "notify", "check_interval":
"P1D", "channel": "stable"}`.

### `.animus/workflows.yaml` and `.animus/workflows/*.yaml`

These YAML files are the editable workflow source of truth for a project.

Typical uses:

- define repo-specific workflow ids such as `standard-workflow`
- define the repository's default workflow explicitly
- declare project MCP servers, agents, variables, phases, and workflow definitions

#### Workflow YAML capabilities

Workflow YAML is more than per-workflow phase plumbing — three top-level blocks
extend the daemon itself:

- **`schedules`** — cron-driven workflow dispatch (UTC; forward-only, missed runs
  are not replayed). See [Workflow YAML: schedules](workflow-yaml.md#schedules).
- **`triggers`** — event-driven dispatch from file watchers, generic webhooks,
  GitHub webhooks, or trigger backend plugins. See
  [Workflow YAML: triggers](workflow-yaml.md#triggers).
- **`daemon`** — workflow-YAML-side daemon settings: `auto_run_ready`,
  `active_hours`, `phase_routing`, and `mcp`. Other `DaemonConfig` keys exist
  on the struct but are configured elsewhere — `pool_size` and `interval_secs`
  are read from the persisted daemon project config or CLI flags, while
  `max_task_retries` and `retry_cooldown_secs` currently have no runtime sink
  at all (round-trip-only). The daemon git/merge policy keys (`auto_merge`,
  `auto_pr`, `auto_commit_before_merge`, `auto_prune_worktrees`) were removed
  in v0.5.x — merge/PR behavior now lives in workflow `post_success.merge`,
  executed by the workflow runner plugin. See
  [Workflow YAML: daemon](workflow-yaml.md#daemon).

All three are optional. They live at the top level of any
`.animus/workflows.yaml` or `.animus/workflows/*.yaml` file. `schedules:` and
`triggers:` entries merge across files by `id` (later overlays override earlier
entries). The `daemon:` block does **not** field-merge — any overlay that
defines `daemon:` replaces the previously-accumulated block wholesale, so keep
`daemon:` in a single file.

#### Hot-reload

When the daemon is running, edits to `.animus/workflows.yaml` and any file under
`.animus/workflows/*.yaml` are picked up automatically by a filesystem watcher
(the `notify` crate's recommended backend per platform). The daemon also re-reads
the runtime-reconfigurable settings in
`~/.animus/<repo-scope>/daemon/pm-config.json` once per scheduler tick (see
[Daemon scheduler timing](#daemon-scheduler-timing)), so `animus daemon config`
changes apply without a restart. `.animus/plugins.lock` and daemon transport
settings still require a daemon restart.

Behaviour:

- A 500 ms debounce coalesces editor write-bursts (vim's `.swp`+rename, vscode's
  atomic temp-then-rename) into a single reload.
- On success: the most-recently compiled `WorkflowConfig` is stored in an
  `arc-swap` snapshot and the daemon broadcasts a `config_reloaded` event on the
  `workflow/events` control subscription. Subscribers see
  `{workflow_id: "<daemon>", kind: "config_reloaded", payload: {...}}`.
- On failure: the daemon logs the YAML diagnostic, broadcasts a
  `config_reload_failed` event, and **keeps the prior snapshot active**. A
  malformed edit never crashes the daemon. Note that daemon paths today still
  read directly from disk via `load_workflow_config_with_metadata` for each
  workflow run, so a malformed-then-saved overlay can still surface a parse
  error to the next workflow that starts; the snapshot guarantees the
  hot-reload broadcast contract and the carry-over for callers that consume
  the snapshot directly. Routing the rest of the daemon's workflow paths
  through the snapshot is tracked separately.
- Daemon transport settings (control socket, MCP, plugin process layout) are
  **not** affected by a reload — those still require a daemon restart.

macOS note: `notify`'s recommended backend on macOS is FSEvents, which delivers
file-system events that are already coarsened by the kernel. A single editor
write often surfaces as a single batched event regardless of the userland write
pattern, so the 500 ms debounce window is largely redundant there but kept for
parity with Linux's finer-grained inotify and Windows' ReadDirectoryChangesW.

Manual trigger (useful when the host filesystem's notify backend is flaky):

```bash
animus workflow config reload [--json]
```

JSON envelope shape (matches the `animus.cli.v1` wrapper):

- success: `{"reloaded": true, "phase_definitions": N, "workflows": M, "agent_profiles": K, "source_files": [...], "config_hash": "<sha256 of compiled workflow config>"}`
- failure: `{"reloaded": false, "errors": [{"message": "<diagnostic>"}], "source_files": [...]}`

### `.animus/plugins/<pack-id>/`

Project-local pack overrides. Use this when a repository needs to override installed pack content without changing Animus globally.

The parent `.animus/plugins/` directory is also scanned as the project-local
plugin discovery directory, so the same tree can contain both plugin binaries
and pack override folders.

### `.animus/plugins.lock`

Project-local plugin integrity lockfile. When plugin install/update flows are
scoped to the repository, Animus records installed plugin versions and sha256
digests here instead of falling back to the global `~/.animus/plugins.lock`.

Each `[[plugins]]` entry records:

- `name`, `version`, `artifact_sha256`, `signature_bundle_sha256`,
  `installed_at` — integrity audit trail.
- `installed_kind` (v0.5.7+) — user-facing kind the SubjectRouter
  dispatches against (e.g. `task`, `task-2`, `archive`). Set by the
  install pipeline; auto-incremented on collision unless the operator
  passes `animus plugin install --as-kind <KIND>`.
- `native_kind` (v0.5.7+) — plugin-native kind declared in the
  manifest (e.g. `task`). The SubjectRouter translates
  `<installed_kind>/<verb>` → `<native_kind>/<verb>` at the wire
  boundary.

Entries written by pre-v0.5.7 installers parse cleanly; consumers treat
the missing fields as `installed_kind == native_kind`. See
[Plugin kind translator (v0.5.7)](../architecture/plugin-kind-translator-v0.5.7.md).

### Project vs global plugin installs

`animus plugin install` supports two install scopes; each scope owns a
matching (install dir, registry, lockfile) triple:

| | Global (default) | Project (`--project`) |
|---|---|---|
| Binaries | `~/.animus/plugins/` (or `$ANIMUS_PLUGIN_DIR` / `--plugin-dir`) | `<project>/.animus/plugins/` |
| Registry | `~/.animus/plugins.yaml` | `<project>/.animus/plugins.yaml` |
| Lockfile | `~/.animus/plugins.lock` (project lockfile preferred when `.animus/` exists) | `<project>/.animus/plugins.lock` |

`--project` is mutually exclusive with `--plugin-dir`, and the same flag
exists on `animus plugin uninstall` and `animus plugin update`. Project
installs run the identical integrity pipeline as global ones: sha256
verification, cosign signature policy, publisher TOFU, and lockfile
fail-closed checks.

**Shadowing rule.** Discovery scans the project-local
`<project>/.animus/plugins/` tier FIRST, so on a name collision the
project-local install wins over both a registry-recorded global install and
a bare binary in the global install dir. `animus plugin list` marks each row
with its `scope` (`project` / `global`) and surfaces hidden global binaries
in a `shadowed` array. The per-project plugin scope file
(`.animus/plugin-scope.yaml`) filters project-installed plugins exactly as it
does global ones — an allowlist that omits a project-installed plugin
excludes it from discovery.

**Version control.** Project plugin BINARIES should not be committed —
`animus init` and `animus plugin install --project` write a
`.animus/.gitignore` covering `plugins/`. The project lockfile
(`.animus/plugins.lock`) SHOULD be committed: that is how a repository pins
its own plugin set, and `animus plugin lock verify` (which sweeps both the
global and project lockfile roots by default) turns the committed lock into
a CI tamper/drift gate. The project registry (`.animus/plugins.yaml`) is
also safe to commit; it carries install provenance, not secrets.

## Repo-Scoped Runtime Config

Animus stores mutable project runtime config under `~/.animus/<repo-scope>/`.

Key files:

- `config/state-machines.v1.json`
- `config/workflow-config.v2.json`
- `config/agent-runtime-config.v2.json`
- `state/pack-selection.v1.json`
- `daemon/pm-config.json`
- `runner/config.json`
- `resume-config.json`

These files are Animus-managed state. Treat them as runtime data, not hand-authored config.

### `daemon/pm-config.json`

Persisted daemon runtime settings, written by `animus daemon config` (CLI) or
`animus.daemon.config-set` (MCP) — never project-local `.animus/`. The
runtime-reconfigurable keys are `pool_size`, `interval_secs`,
`max_tasks_per_tick`, `auto_run_ready`, `stale_threshold_hours`, and
`phase_timeout_secs`; the running daemon re-reads them once per scheduler tick,
so changes apply without a restart. Exception: the daemon's `ProcessManager`
captures `phase_timeout_secs` once at startup, so a changed phase timeout only
takes effect for the live process manager after a daemon restart (the per-tick
reload updates the scheduler options only). Unknown keys (including fields removed in
earlier releases, such as the v0.5.x daemon git/merge policy keys and the no-op
`idle_timeout_secs`) round-trip untouched so old files keep loading.

## Daemon scheduler timing

The daemon's scheduling model has two layers:

- **Ticks are interval-driven.** The scheduler loop sleeps `interval_secs`
  between ticks (default 5, clamped to a minimum of 1 second). Each tick
  re-reads `daemon/pm-config.json`, runs housekeeping, evaluates schedules and
  triggers, and dispatches ready work.
- **Phases within a running workflow are reactive.** Once a workflow is
  dispatched, phase completions are consumed as they arrive; the next phase
  does not wait for the next tick.

Cron schedules (`schedules:` in workflow YAML) fire on the first tick at or
after their occurrence, so punctuality is bounded by `interval_secs`. A
catch-up horizon (10 minutes) absorbs long ticks or `interval_secs` well above
60 with at most one catch-up fire; occurrences missed for longer (daemon
stopped, `active_hours` gate closed) are skipped, not replayed.

Two distinct knobs bound dispatch:

- **`pool_size`** — hard cap on concurrently active agents/workflow runners.
  The effective cap is `min(pool_size, ANIMUS_WORKFLOW_CONCURRENCY_MAX)`
  (quota default 10); when `pool_size` is unset, the quota alone applies.
- **`max_tasks_per_tick`** — how many *new* workflows one tick may dispatch
  (default 2). Per tick the dispatch budget is
  `min(max_tasks_per_tick, pool capacity - active agents)`, and schedules and
  triggers draw from that same shared budget.

So `pool_size` limits steady-state concurrency while `max_tasks_per_tick`
limits ramp-up rate: with `pool_size 8`, `max_tasks_per_tick 2`, and an empty
pool, reaching 8 concurrent workflows takes at least 4 ticks.

## Global User Config

### `~/.animus/config.json`

The global Animus config stores machine-local user settings such as:

- agent runner auth token
- user-defined MCP server entries
- Claude profile launch environments

Use `ANIMUS_CONFIG_DIR` to override the global config root in tests or custom environments.

Example:

```json
{
  "claude_profiles": {
    "main": {
      "env": {
        "CLAUDE_CONFIG_DIR": "/Users/alice/.claude-main"
      }
    }
  }
}
```

## Installed Sources

### Machine-installed packs

Installed packs live at:

```text
~/.animus/packs/<pack-id>/<version>/
```

Manage them with:

```bash
animus pack list
animus pack info --pack-id animus.task
animus pack install --path /tmp/vendor.pack --activate
animus pack pin --pack-id vendor.pack --version =1.2.3
```

## Configuration Precedence

Behavior resolves in this order:

1. CLI flags
2. supported environment variables
3. project-local pack overrides in `.animus/plugins/<pack-id>/`
4. project YAML in `.animus/workflows.yaml` and `.animus/workflows/*.yaml`
5. installed packs in `~/.animus/packs/`

## Secrets vs. Non-Secret Config

> **Plugin-owned credentials still stay out of YAML.**
> As of v0.5.8, plugins can read required env vars from either the
> daemon's process environment or the project-scoped keychain-backed
> secret store. Resolution order is: explicit parent-process env first,
> then the keychain entry for the current `repo-scope`.
>
> For credentials that a *phase* or *MCP server* declared in YAML needs
> to receive (e.g. an MCP server's `env:` block), v0.5.5 adds the
> [`secrets:` block](./workflow-yaml.md#secrets-v055) — declare the
> mapping once, reference with `${secret.<name>}`, and the compiler
> resolves it at config-compile time using the same env-first,
> keychain-second lookup chain.
>
> Use `${VAR}` interpolation for non-secret configuration that varies by environment:
> API base URLs, team IDs, feature flags, channel allowlists.

### Where credentials live

| Plugin | Env var it reads | Set on |
|---|---|---|
| `animus-subject-linear` | `LINEAR_API_TOKEN` | Parent-process env, else `animus secret` keychain entry |
| `animus-provider-oai` | `OPENAI_API_KEY` (and provider-specific overrides) | Parent-process env, else `animus secret` keychain entry |
| `animus-provider-claude` | inherits the Claude CLI's existing auth | The daemon's process environment |
| `animus-provider-codex` | inherits the Codex CLI's existing auth | The daemon's process environment |
| future plugins | declared in the plugin's `plugin.toml` `[[env]]` section or its README | Parent-process env, else `animus secret` keychain entry when the plugin declares that variable |

Seed credentials either by exporting them before daemon start or by
storing them with `animus secret set <KEY>`. Explicit process env still
wins on collision. Example:

```bash
LINEAR_API_TOKEN=lin_api_... OPENAI_API_KEY=sk-... animus daemon start
```

```bash
animus secret set LINEAR_API_TOKEN --value lin_api_...
animus secret set OPENAI_API_KEY --value sk-...
animus daemon start
```

### Workflow YAML interpolation (non-secret config)

Workflow YAML supports shell-style `${VAR}` interpolation **for non-secret config only** so
the same YAML can target dev, staging, and prod without edits. Substitution is performed by
the workflow YAML loader **before** the YAML is parsed, so every string scalar in the file
(`.animus/workflows.yaml`, `.animus/workflows/*.yaml`, and pack-shipped workflow overlays)
accepts the same syntax uniformly. Lookup order is `std::env` first, then the project-scoped
keychain-backed secret store installed by `animus secret`.

#### Syntax

| Form | Meaning |
| --- | --- |
| `${VAR}` | Required. Errors with file path + line number if `VAR` is unset. |
| `${VAR:-default}` | Optional. Falls back to the literal `default` when `VAR` is unset or empty. |
| `${VAR:?message}` | Required with a custom error message. |
| `$$` | Literal `$`. |

A lone `$` not followed by `{` or `$` passes through unchanged, so prose like `cost $5` is
preserved.

Env var names follow POSIX shell rules — they must start with a letter or underscore and
contain only letters, digits, and underscores. The loader rejects invalid names with the
offending file path and line number.

#### Example

```yaml
# CORRECT — non-secret config in YAML, secrets from plugin env
subjects:
  - id: my-linear
    backend: linear
    config:
      team_id: ${LINEAR_TEAM_ID:-default-team-uuid}
      api_url: ${LINEAR_API_URL:-https://api.linear.app/graphql}
      workspace: ${LINEAR_WORKSPACE:?set LINEAR_WORKSPACE for this workflow}

# LINEAR_API_TOKEN is set in the daemon's environment, NOT here.
# animus-subject-linear reads it directly from its process env at startup.
# Run the daemon with: LINEAR_API_TOKEN=lin_api_... animus daemon start
```

```yaml
# WRONG — DO NOT DO THIS
subjects:
  - id: my-linear
    backend: linear
    config:
      api_token: ${LINEAR_API_TOKEN}   # secrets should not be in YAML at all
```

#### Error reporting

When a required `${VAR}` is unset, the loader fails fast with a message that cites the YAML
file path and 1-based line number:

```
workflow YAML at .animus/workflows/agents.yaml line 12 references unset env var LINEAR_TEAM_ID.
```

#### `.env` files

The daemon does **not** auto-load `.env` files. To pre-load variables before starting the
daemon, source them in the parent shell:

```bash
set -a; source .env; set +a
animus daemon start
```

Or use a process supervisor (systemd, launchd, docker-compose) that supports an
`EnvironmentFile` directive.

For persistent machine-local storage without keeping plaintext in the shell
environment, use `animus secret import-env` to migrate selected `.env`
entries into the OS keychain, then remove the plaintext file.

#### What gets interpolated

Every string scalar in the YAML — including subject configs, provider tokens, workflow
metadata, env override blocks, and any future plugin-config field — is substituted. Numeric
and boolean YAML values pass through unchanged. Comments are not stripped before
substitution, so `# ${VAR}` inside a comment is also substituted; this is intentional and
matches docker-compose semantics.

### External prompt files

Long agent system prompts can live in their own files outside `.animus/workflows.yaml`.
Each `AgentProfile` accepts an optional `system_prompt_file` that points at a UTF-8 text
file:

- Relative paths resolve against the source YAML file's parent directory; absolute paths
  are taken as-is.
- The file is read at config-compile time and inlined verbatim into the agent's
  `system_prompt` (no trimming or normalization). The compiled
  `workflow-config.v2.json` is self-contained — `system_prompt_file` is dropped after
  resolution.
- `system_prompt` (inline) and `system_prompt_file` are mutually exclusive on the same
  agent. Setting both fails with an error naming the agent id, the source YAML file, and
  (when detectable) the line number.
- Missing files, non-UTF-8 contents, and files larger than 1 MiB cause compile to fail
  with the resolved absolute path in the error.

```yaml
agents:
  implementer:
    description: "Implementer"
    system_prompt: "You are the implementer. Keep changes minimal."

  researcher:
    description: "Researcher"
    system_prompt_file: prompts/researcher.md
```

### HTTP MCP servers

The top-level `mcp_servers:` map in workflow YAML declares MCP servers that agents
can attach to. Each entry must pick exactly one transport:

- `transport: http` requires `url:` (an `http://` or `https://` endpoint). `command`,
  `args`, and `env` must not be set on http entries.
- `transport: stdio` (the default when omitted) requires `command:`; optional `args:`
  and `env:` are supported. `url` must not be set on stdio entries.

Agents reference servers by name via `agents.<id>.mcp_servers: [name1, name2]`. The
name must match a key in the top-level `mcp_servers:` map; an unresolved name fails
config validation with a clear error.

Env-var interpolation applies to the entire workflow YAML before parsing, so it
reaches `url:` and `command:` fields too — useful for swapping endpoints per
environment:

```yaml
mcp_servers:
  robinhood-trading:
    transport: http
    url: ${ROBINHOOD_MCP_URL:-https://agent.robinhood.com/mcp/trading}
  animus:
    transport: stdio
    command: animus
    args: ["mcp", "serve"]

agents:
  trader:
    description: "Trade execution agent"
    mcp_servers:
      - robinhood-trading
      - animus
```

The compiled `workflow-config.v2.json` carries the resolved server definitions, and
each phase's runtime contract surfaces them under `/mcp/additional_servers` in the
shape the agent runner expects (a `url` field for http transport, `command`/`args`/
`env` for stdio).

Note: Animus launches Claude with `--strict-mcp-config`, so user-scope `claude mcp
add` registrations are not visible to Animus-spawned sessions. Servers an agent
needs to call must be declared in workflow YAML (or supplied by a pack overlay or
project-level `mcp_servers` config).

#### HTTP MCP OAuth (v0.5.5)

HTTP-transport MCP servers can attach an `oauth:` block so the daemon resolves
a bearer token and injects an `Authorization: Bearer <token>` header into the
additional MCP server entry passed to the agent. Tokens never appear in YAML —
all credential material is read from process environment variables named via
`*_env` pointers in the block. See the
[workflow-yaml HTTP transport with OAuth](workflow-yaml.md#http-transport-with-oauth-v055)
section for the full shape and validation rules.

Token cache: `~/.animus/<repo-scope>/mcp-oauth-cache/<server>.json` with
`0600` permissions on Unix. The daemon reads `client_id` / `client_secret` /
`refresh_token` / `bearer` env vars from the daemon process's own
environment, mirroring how plugin secrets are sourced today (see
`docs/reference/configuration.md#secrets-vs-non-secret-config`).

## Self-update

The `animus` CLI can poll its own GitHub releases
(`launchapp-dev/animus-cli`) on startup and surface, prompt for, or
silently apply newer builds. The behavior is fully configurable via the
`auto_update` block in `.animus/config.json` (or the global
`~/.animus/config.json`):

```jsonc
{
  "auto_update": {
    "mode": "notify",          // off | notify | prompt | auto
    "check_interval": "P1D",   // ISO-8601 duration; default daily
    "channel": "stable"        // stable | prerelease
  }
}
```

Modes:

- `off` — never check, never notify, never apply.
- `notify` (default) — print a one-liner to stderr when a newer release is
  available, but do nothing else.
- `prompt` — ask `[y/N]` on stderr when a newer release is available. When
  stdin is not a TTY (CI, piped invocations) this degrades to `notify`.
- `auto` — download and atomically install the newer release in the
  background. The currently-running invocation continues to run on the
  old binary; the swap takes effect on the next invocation.

`check_interval` accepts an ISO-8601 duration string (`P1D`, `PT6H`,
`P1W`, `PT30M`, ...). The last-check timestamp persists at
`~/.animus/auto-update-state.json`.

`channel` filters releases: `stable` drops anything tagged
`prerelease: true` on GitHub; `prerelease` admits both. Pass
`animus self update --prerelease` to override per-invocation.

Manual trigger: `animus self update` bypasses the mode check and applies
the newest matching release. Flags:

- `--check-only` — print the available version and exit 0 if newer,
  non-zero otherwise. Useful in CI.
- `--force` — re-install the latest tag even when already current
  (repair).
- `--prerelease` — include prereleases regardless of `channel`.
- `--yes` — skip the interactive confirmation prompt.

Integrity: when the GitHub release asset has an inline `digest:
sha256:<hex>` field, the staged download is verified against it before
the atomic swap. Releases that ship a sidecar `<asset>.sha256` are also
honored. Releases that do neither are downloaded but not hash-verified —
operators are encouraged to publish digests upstream.

The startup check is fire-and-forget — it runs as a tokio background
task and never delays the subcommand the user actually asked for.
Network failures during the background check are silently dropped; run
`animus self update --check-only` to surface them.

## Environment Variables

The complete v0.4.0 env var surface was renamed from `AO_*` to `ANIMUS_*`. There are no
legacy aliases; the old names will not be read.

### Core

| Variable | Description |
|---|---|
| `ANIMUS_CONFIG_DIR` | Override the global Animus config directory (default `~/.animus`) |
| `ANIMUS_DISABLE_CI_CACHE` | Truthy (`1`, `true`, `yes`, `on`) — disable the per-project CI status cache used by `animus status` (`~/.animus/<repo-scope>/cache/ci-status.json`), forcing a fresh `gh` lookup every call |
| `ANIMUS_CI_CACHE_TTL_SECS` | TTL in seconds for the CI status cache (default 60). Honored on every read, so lowering it immediately invalidates older entries |
| `ANIMUS_MCP_SCHEMA_DRAFT` | Select Draft-07 MCP tool input schemas |
| `ANIMUS_MCP_ENDPOINT` | Override the MCP server endpoint for the CLI's embedded client |
| `ANIMUS_USER_ID` | Override the recorded user id for authored actions |
| `ANIMUS_ASSIGNEE_USER_ID` | Override the assignee user id when creating tasks |
| `ANIMUS_DEBUG` | Enable verbose debug logging across the CLI and daemon |
| `ANIMUS_LOG_JSON` | Emit log lines as JSON for log shippers |
| `ANIMUS_DEBUG_MCP_STDIO` | Log raw MCP stdio frames for plugin/server debugging |
| `ANIMUS_AUTO_UPDATE_MODE` | Override the configured `auto_update.mode` for the current process (`off`, `notify`, `prompt`, `auto`) |
| `ANIMUS_AUTO_UPDATE_DISABLE` | Short-circuit auto-update to `off` regardless of config when set to a truthy value |
| `ANIMUS_METRICS_DISABLE` | Truthy — hard kill switch for opt-in anonymous metrics. Suppresses emission regardless of any persisted opt-in. See [Opt-in anonymous metrics](#opt-in-anonymous-metrics-v053). |

### Plugins and templates

| Variable | Description |
|---|---|
| `ANIMUS_PLUGIN_DIR` | Override the global plugin install directory used by `animus plugin install`. Discovery always scans the resolved global install dir: `$ANIMUS_PLUGIN_DIR` when set, otherwise `~/.animus/plugins/`. `--plugin-dir <PATH>` overrides it for install/uninstall commands |
| `ANIMUS_PLUGIN_PATH` | Colon-separated list of additional directories to scan for `animus-provider-*` and `animus-plugin-*` binaries during plugin discovery |
| `ANIMUS_TEMPLATE_REGISTRY_URL` | Override the default template registry URL used by `animus init`. Defaults to the LaunchApp project-templates registry |

### Runner and workflow

| Variable | Description |
|---|---|
| `ANIMUS_SKIP_RUNNER_START` | Skip auto-starting the runner alongside the daemon |
| `ANIMUS_AUTO_REBUILD_RUNNER_ON_MAIN_UPDATE` | Rebuild the runner binary when main is updated |
| `ANIMUS_RUNNER_BUILD_ID` | Pin a specific runner build id |
| `ANIMUS_WORKFLOW_RUNNER_BIN` | Override the workflow-runner binary path |
| `ANIMUS_PHASE_RUN_ATTEMPTS` | Maximum attempts for a single phase run before giving up |
| `ANIMUS_PHASE_MAX_CONTINUATIONS` | Cap on phase continuation rounds |
| `ANIMUS_REPLAY_SESSION` | Path to a recorded decision log; when set (non-empty), the workflow runner replays provider decisions from that session instead of invoking the live provider |
| `ANIMUS_RUNNER_SESSION_DIR` | Override the runner-sessions sidecar directory used to look up provider session ids for resume (default `~/.animus/runner-sessions/`) |
| `ANIMUS_COST_STATE_ROOT` | Override the root directory for `cost-state.v1.json` and `decisions.jsonl`. Test seam — production callers leave it unset and the standard `~/.animus/<repo-scope>/` root is used |

### Daemon runtime quotas

Per-process caps the daemon resolves once at startup (`RuntimeQuotas::from_env`).
Unset, empty, non-numeric, or `0` values fall back to the default, so an
accidental empty override cannot disable a cap.

| Variable | Default | Description |
|---|---|---|
| `ANIMUS_TRIGGER_BACKLOG_MAX` | 1000 | Max unprocessed webhook/trigger events retained per trigger; the oldest are dropped (with a warning) beyond this |
| `ANIMUS_SUBSCRIBER_MEMORY_MAX_MB` | 10 | Approximate per-subscriber buffer cap (MB) for `workflow/events` subscriptions; a lagging subscriber is terminated with reason `buffer_full_lagged` |
| `ANIMUS_PLUGIN_PROCESS_MAX` | 50 | Max concurrently-spawned plugin child processes; further spawns are refused with an error |
| `ANIMUS_WORKFLOW_CONCURRENCY_MAX` | 10 | Max workflow runner subprocesses dispatched in parallel; excess requests queue until headroom appears. Also upper-bounds `pool_size` (see [Daemon scheduler timing](#daemon-scheduler-timing)) |

### Notifications

| Variable | Description |
|---|---|
| `ANIMUS_NOTIFY_WEBHOOK_URL` | Common env var referenced by generic webhook connectors via `url_env: "ANIMUS_NOTIFY_WEBHOOK_URL"` |
| `ANIMUS_NOTIFY_BEARER_TOKEN` | Common bearer-token env var referenced from `headers_env`, for example `"headers_env": { "Authorization": "ANIMUS_NOTIFY_BEARER_TOKEN" }` |
| `ANIMUS_NOTIFY_MISSING_URL` | Behavior tuning for notification dispatch when the webhook URL is unset |

Any other env var name can be referenced by a notification config's `url_env` or per-header
env lookups, so projects can define their own `ANIMUS_NOTIFY_*` variables and reference
them from the persisted notification config.

Notification config is persisted under the repo-scoped daemon runtime file
`~/.animus/<repo-scope>/daemon/pm-config.json`, not under project-local
`.animus/`. The daemon forwards only the env var names explicitly referenced by
that `notification_config` block's `*_env` fields (including object-valued
`headers_env` maps) into the notifier plugin subprocess, so unrelated daemon
credentials are not re-exposed to notifier plugins.

### Provider plugin tuning

These passthrough variables are read by the installed provider plugins (claude, codex,
gemini, opencode) when spawning their underlying CLIs.

| Variable | Description |
|---|---|
| `ANIMUS_CLAUDE_BYPASS_PERMISSIONS` | Pass `--dangerously-skip-permissions` (or equivalent) to the Claude provider plugin. Intended for unattended test environments only — never set in production |
| `ANIMUS_CLAUDE_EXTRA_ARGS` / `_JSON` | Extra args to forward to the Claude CLI |
| `ANIMUS_CODEX_EXTRA_ARGS` / `_JSON` | Extra args to forward to the Codex CLI |
| `ANIMUS_CODEX_EXTRA_CONFIG_OVERRIDES` / `_JSON` | Extra `--config` overrides for Codex |
| `ANIMUS_CODEX_NETWORK_ACCESS` | Toggle Codex sandbox network access |
| `ANIMUS_CODEX_WEB_SEARCH` | Toggle Codex web search capability |
| `ANIMUS_GEMINI_EXTRA_ARGS` / `_JSON` | Extra args to forward to the Gemini CLI |
| `ANIMUS_OPENCODE_EXTRA_ARGS` / `_JSON` | Extra args to forward to the OpenCode CLI |
| `ANIMUS_AI_CLI_EXTRA_ARGS` / `_JSON` | Generic CLI passthrough used by the wrapper crate |

### External / inherited

| Variable | Description |
|---|---|
| `CLAUDECODE` | Set by the Claude Code harness when an embedded session is active. Animus detects it and unsets it before spawning a nested `claude` CLI to avoid the "cannot launch inside another Claude Code session" guard |

## Plugin kill-switches

These environment variables are operator escape hatches for shutting down a
plugin subsystem when something installed has gone bad and you need the
daemon to keep running while you investigate. They require a daemon restart
to take effect and another restart to re-enable plugin dispatch after you
clear the variable.

| Variable | Description |
|---|---|
| `ANIMUS_DAEMON_DISABLE_TRIGGERS` | Truthy (`1`, `true`, `yes`, `on`) — skips the trigger plugin supervisor on daemon start AND interrupts any in-progress backoff sleeps so a flapping plugin stops respawning immediately. Useful when a trigger plugin is panicking, flooding events, or wedging the daemon's startup. Daemon restart needed to re-enable. |
| `ANIMUS_DAEMON_DISABLE_SUBJECT_PLUGINS` | Truthy — skips subject plugin discovery. Subject plugin calls then behave as if no backend is installed, so most subject operations return not-found/method-not-found. Useful for isolating a broken subject backend. |
| `ANIMUS_DAEMON_DISABLE_LOG_STORAGE_PLUGIN` | Truthy — ignores installed `log_storage_backend` plugins and uses the in-tree `logs/events.jsonl` backend. |
| ~~`ANIMUS_PROVIDER_DISABLE_PLUGIN`~~ | **Removed in v0.4.12.** Previously forced the `SessionBackendResolver` to fall back to in-tree provider backends. The in-tree backends were extracted to standalone `launchapp-dev/animus-provider-*` plugins in v0.4.12, so there is nothing left to fall back to. Setting this variable has no effect. If a provider plugin is misbehaving, uninstall it with `animus plugin uninstall --name <name>` or remove/quarantine the binary from the plugin directory. |

Error surfaces emit a hint pointing at the right kill-switch when a plugin
fails its handshake or exhausts its restart budget, so operators don't have
to read the source to find these during an incident.

When a provider plugin is not installed for the tool a workflow phase asks
for, the resolver surfaces a hard error like:

```
Provider plugin 'claude' not installed. Install with:
  animus plugin install launchapp-dev/animus-provider-claude --allow-shadow-builtin
Or run: animus plugin install-defaults
```

This is intentional — silent fallback to a removed in-tree backend would
hide broken or missing plugins during daemon startup.

## Opt-in anonymous metrics (v0.5.4)

Animus ships a minimal opt-in telemetry surface. It is **disabled by
default**: the first time you run `animus init` or `animus daemon
start` in a TTY, the CLI prompts once for consent. The answer is
persisted to the **user-global** config at
`~/.animus/config.json` (overridable via `ANIMUS_CONFIG_DIR`) so you
never see the prompt again across any project on this machine. The
opt-in is intentionally **never** stored in the project-local
`.animus/config.json` — that file can be committed, and a cloned repo
must never carry someone else's consent or point at a third-party
endpoint. Non-interactive contexts (no TTY) default to **opt-out**
with no prompt.

### Config block (in `~/.animus/config.json`)

```json
{
  "metrics": {
    "enabled": true,
    "endpoint": "https://metrics.animus.dev/v1/events",
    "batch_interval": "P1D",
    "install_id": "9e2c1bcc-4a2f-4f24-bf25-9c0d0d3f6e2c"
  }
}
```

| Field | Meaning |
|---|---|
| `enabled` | `true` = opted in, `false` = opted out, absent block = never asked. |
| `endpoint` | HTTP receiver for batched event-counter payloads. |
| `batch_interval` | ISO 8601 duration. Default `P1D` (daily). |
| `install_id` | Opaque random UUID generated on opt-in. Wiped on opt-out. |

### Kill switch

| Variable | Description |
|---|---|
| `ANIMUS_METRICS_DISABLE` | Truthy (`1`, `true`, `yes`, `on`) — suppresses all telemetry regardless of config. Overrides any persisted opt-in. |

### Privacy invariants

These properties are enforced at the type level; new event types
require updating the closed enums in
`crates/orchestrator-cli/src/services/metrics/events.rs`:

- No file paths, repo names, branch names, or git URLs.
- No prompt content, model responses, agent output, or generated text.
- No environment-variable values.
- No credentials, API keys, or tokens.
- No subject IDs (task ids, requirement ids).
- Every event tag is a compile-time enum variant; payloads cannot
  carry user-supplied strings.

### Payload shape

```json
{
  "install_id": "<uuid>",
  "animus_version": "0.5.3",
  "os": "darwin",
  "arch": "aarch64",
  "events": [
    {
      "name": "workflow_started",
      "tags": {"workflow_kind": "task"},
      "count": 5,
      "first_seen": "2026-06-04T10:00:00Z"
    }
  ]
}
```

Counter events are batched on disk under
`~/.animus/<repo-scope>/metrics/pending.jsonl`; in-flight sends rotate
that buffer into `flushing-<ts>-<uuid>.jsonl`, and successful sends
update `last-send.txt`. Events are flushed by `animus daemon metrics flush` or
the next CLI invocation. Failed sends retry with exponential backoff (3
attempts) before being preserved in the pending queue for the next
attempt — CLI/daemon operations are never blocked on the send path.

The pending buffer is intentionally bounded because telemetry is
best-effort. Current guards in `crates/orchestrator-cli/src/services/metrics/recorder.rs` are:

- record-time soft cap: once `pending.jsonl` reaches 8 MiB, new events are dropped instead of letting the buffer grow without bound
- read/flush hard cap: any single metrics file above 16 MiB is treated as pathological and is not read into memory
- stale `flushing-*.jsonl` recovery: oversized abandoned flush snapshots are deleted instead of being folded back into the next batch
- manual reclaim sweep: `animus daemon metrics cleanup` sweeps every repo scope under `~/.animus/`, deleting stale or oversized `flushing-*.jsonl` files and dropping oversized `pending.jsonl` buffers

This trades away some telemetry during a stalled or racing flush in exchange
for keeping ordinary CLI commands memory-safe.

### CLI surface

| Command | Effect |
|---|---|
| `animus daemon metrics status` | Show enabled flag, install_id, pending event count, last-send timestamp. |
| `animus daemon metrics enable` | Opt in (no re-prompt). Generates a new `install_id` if missing. |
| `animus daemon metrics disable` | Opt out and drop any buffered events. |
| `animus daemon metrics flush` | Force-send buffered events (debug). |
| `animus daemon metrics cleanup` | Sweep all scoped metrics dirs for orphaned/oversized flushing snapshots and oversized pending buffers. |

## Cost governance and model rates (v0.5.5)

Animus tracks per-workflow + per-phase token + USD spend using the
`AgentRunEvent::Metadata { cost, tokens }` events the provider
plugins already emit. The rollups live under
`~/.animus/<scope>/cost-state.v1.json` (schema
`animus.cost-state.v1`) and budget-exceeded decision records are
appended to `~/.animus/<scope>/decisions.jsonl` (schema
`animus.budget-exceeded.v1`).

When a provider does not report `cost` on the metadata frame, the
aggregator estimates USD from a small per-model rate table
(`crates/orchestrator-cli/src/services/cost/model_rates.rs`).
The table is intentionally simple — a single combined input + output +
reasoning rate per model family — and reflects published vendor pricing
as of the release that contains this file.

| Model prefix | USD per 1M combined tokens |
|---|---|
| `claude-opus` | 30.00 |
| `claude-sonnet` | 6.00 |
| `claude-haiku` | 1.25 |
| `codex` | 5.00 |
| `o4` | 5.00 |
| `gpt-5` | 5.00 |
| `gemini-3` | 1.25 |
| `gemini-2` | 0.75 |
| `kimi` | 1.00 |
| `minimax` | 0.70 |
| `opencode` | 2.50 |

Unknown model prefixes produce `None` (no estimate) rather than a
fabricated number. Vendor-reported `cost` on the wire always wins over
the table. To declare cost caps, see the `budget:` section in
[`workflow-yaml.md`](workflow-yaml.md). To inspect spend, see
[`cli/index.md`](cli/index.md#cost).

## Scheduler wake model

The daemon's main loop is event-driven; the legacy fixed-interval polling
loop is gone. A dispatch pass runs when any of these fire:

1. **Events (`daemon/nudge`)** — the CLI write paths `animus subject
   create/update/status` and `animus queue enqueue/release` (and their MCP
   equivalents, which execute the same CLI handlers) send a best-effort
   `daemon/nudge` control message after a successful mutation. The same
   wake fires in-process for control-socket writes (web UI), on
   workflow/phase completion events from running workflow-runners, and
   after a workflow-config hot-reload. Nudges are fire-and-forget: if the
   daemon is not running, or predates the `daemon/nudge` method, the
   sender silently ignores the failure and the next heartbeat picks the
   work up. Bursts coalesce — N rapid nudges produce at most one extra
   pass.
2. **Cron deadlines** — the loop computes the earliest upcoming occurrence
   across all compiled `schedules:` and sleeps until exactly that instant,
   so cron fires on time instead of on the next polling tick. The deadline
   is recomputed every pass (and a config reload nudges the loop awake),
   so schedule edits take effect immediately. The catch-up scan (10-minute
   lookback) remains the recovery path for occurrences missed while the
   daemon was busy or down, and while any schedule is enabled the loop
   additionally sweeps at least every 5 minutes (half the catch-up
   horizon) so a fire blocked by a full pool is retried before it falls
   out of the horizon, even when `interval_secs` is much longer.
3. **Fallback heartbeat (`interval_secs`)** — the maximum sleep when no
   event arrives. This is *not* the dispatch latency; it bounds pickup of
   out-of-band state mutations made without the CLI/MCP surfaces, and it
   paces housekeeping: the heavier reconciliation legs (manual-timeout,
   zombie-workflow, stale in-progress sweeps) run at most once per
   heartbeat period, while event wakes run only the dispatch legs
   (schedules, queue drain, ready tasks, completed-process reaping).

Pause/resume gating is unchanged: `animus daemon pause` still gates
dispatch on event wakes, not just heartbeat ticks — a nudge while paused
wakes the loop but dispatches nothing.

## Notes

- Project YAML is the authored workflow surface.
- Animus still ships the `hello-world` walkthrough template plus kernel baseline config for phases and MCP defaults, but current builds do not ship built-in workflow definitions, bundled pack content, or bundled skill fallback.
- `animus init --json` now includes a `recommended_install` object sourced from `crates/orchestrator-cli/config/default-install.json` so callers can surface the default pack and plugin set explicitly.
- Mutable runtime state lives under `~/.animus/<repo-scope>/`.
- The daemon schedules and supervises work; workflow and pack content still define behavior.
