# Configuration Reference

Animus resolves behavior from project YAML, installed pack layers, scoped runtime state, and environment overrides.

## Project-Local Sources

### `.animus/config.json`

Repository-local Animus configuration created during setup.

### `.animus/workflows.yaml` and `.animus/workflows/*.yaml`

These YAML files are the editable workflow source of truth for a project.

Typical uses:

- define repo-specific workflow ids such as `standard-workflow`
- define the repository's default workflow explicitly
- declare project MCP servers, agents, variables, phases, and workflow definitions

### `.animus/plugins/<pack-id>/`

Project-local pack overrides. Use this when a repository needs to override installed pack content without changing Animus globally.

The parent `.animus/plugins/` directory is also scanned as the project-local
plugin discovery directory, so the same tree can contain both plugin binaries
and pack override folders.

### `.animus/plugins.lock`

Project-local plugin integrity lockfile. When plugin install/update flows are
scoped to the repository, Animus records installed plugin versions and sha256
digests here instead of falling back to the global `~/.animus/plugins.lock`.

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
animus pack inspect --pack-id animus.task
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

> **Secrets never go in workflow YAML.** API tokens, passwords, and OAuth keys are
> read directly from the daemon's process environment by the plugin that needs them
> (e.g. `LINEAR_API_TOKEN` is read by `animus-subject-linear`; `OPENAI_API_KEY` by
> `animus-provider-oai`). The workflow YAML configures **which** plugin to use and
> **non-sensitive** parameters — not the credentials themselves.
>
> Use `${VAR}` interpolation for non-secret configuration that varies by environment:
> API base URLs, team IDs, feature flags, channel allowlists.

### Where credentials live

| Plugin | Env var it reads | Set on |
|---|---|---|
| `animus-subject-linear` | `LINEAR_API_TOKEN` | The daemon's process environment |
| `animus-provider-oai` | `OPENAI_API_KEY` (and provider-specific overrides) | The daemon's process environment |
| `animus-provider-claude` | inherits the Claude CLI's existing auth | The daemon's process environment |
| `animus-provider-codex` | inherits the Codex CLI's existing auth | The daemon's process environment |
| future plugins | declared in the plugin's `plugin.toml` `[[env]]` section or its README | The daemon's process environment |

Start the daemon with the necessary env vars exported in its parent shell, for example:

```bash
LINEAR_API_TOKEN=lin_api_... OPENAI_API_KEY=sk-... animus daemon start --autonomous
```

### Workflow YAML interpolation (non-secret config)

Workflow YAML supports shell-style `${VAR}` interpolation **for non-secret config only** so
the same YAML can target dev, staging, and prod without edits. Substitution is performed by
the workflow YAML loader **before** the YAML is parsed, so every string scalar in the file
(`.animus/workflows.yaml`, `.animus/workflows/*.yaml`, and pack-shipped workflow overlays)
accepts the same syntax uniformly.

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
animus daemon start --autonomous
```

Or use a process supervisor (systemd, launchd, docker-compose) that supports an
`EnvironmentFile` directive.

#### What gets interpolated

Every string scalar in the YAML — including subject configs, provider tokens, workflow
metadata, env override blocks, and any future plugin-config field — is substituted. Numeric
and boolean YAML values pass through unchanged. Comments are not stripped before
substitution, so `# ${VAR}` inside a comment is also substituted; this is intentional and
matches docker-compose semantics.

## Environment Variables

The complete v0.4.0 env var surface was renamed from `AO_*` to `ANIMUS_*`. There are no
legacy aliases; the old names will not be read.

### Core

| Variable | Description |
|---|---|
| `ANIMUS_CONFIG_DIR` | Override the global Animus config directory (default `~/.animus`) |
| `ANIMUS_RUNNER_CONFIG_DIR` | Override the runner config directory. Without it, the CLI resolves runner config under `~/.animus/<repo-scope>/runner/` |
| `ANIMUS_RUNNER_SCOPE` | Runner scope identifier. Defaults to the repo-scope derived from the project root |
| `ANIMUS_MCP_SCHEMA_DRAFT` | Select Draft-07 MCP tool input schemas |
| `ANIMUS_MCP_ENDPOINT` | Override the MCP server endpoint for the CLI's embedded client |
| `ANIMUS_USER_ID` | Override the recorded user id for authored actions |
| `ANIMUS_ASSIGNEE_USER_ID` | Override the assignee user id when creating tasks |
| `ANIMUS_DEBUG` | Enable verbose debug logging across the CLI and daemon |
| `ANIMUS_LOG_JSON` | Emit log lines as JSON for log shippers |
| `ANIMUS_DEBUG_MCP_STDIO` | Log raw MCP stdio frames for plugin/server debugging |

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

## Notes

- Project YAML is the authored workflow surface.
- Animus still ships the `hello-world` walkthrough template plus kernel baseline config for phases and MCP defaults, but current builds do not ship built-in workflow definitions, bundled pack content, or bundled skill fallback.
- `animus init --json` now includes a `recommended_install` object sourced from `crates/orchestrator-cli/config/default-install.json` so callers can surface the default pack and plugin set explicitly.
- Mutable runtime state lives under `~/.animus/<repo-scope>/`.
- The daemon schedules and supervises work; workflow and pack content still define behavior.
