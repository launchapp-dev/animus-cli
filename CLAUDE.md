# Animus - Coding Agent Guide

This file is the current working brief for AI coding agents operating in this repo.
If a statement here conflicts with source, update the docs and follow source.

For the v0.5 kernel + flavors commitment (the load-bearing product architecture decision — read this before any scope question), see [docs/architecture/kernel-and-flavors.md](docs/architecture/kernel-and-flavors.md). Animus is a kernel + a default flavor (curated plugin bundle). Future flavors emerge from real customer pull, not roadmap speculation.

For the v0.4.0 naming + plugin extraction, see [docs/architecture/naming-contract.md](docs/architecture/naming-contract.md), [docs/architecture/subject-backend-plugins.md](docs/architecture/subject-backend-plugins.md), and the migration guide at [docs/migration/v0.3-to-v0.4.md](docs/migration/v0.3-to-v0.4.md).

## Verify Before Repeating

This repo has had stale prose before. Verify against these files before restating
architecture, command counts, routes, or state paths:

- `Cargo.toml`
- `docs/reference/cli/index.md`
- `docs/reference/mcp-tools.md`
- `crates/orchestrator-cli/src/cli_types/root_types.rs`
- `crates/orchestrator-core/src/config.rs`
- `crates/orchestrator-core/src/services.rs`
- `crates/protocol/src/config.rs`
- `crates/protocol/src/repository_scope.rs`

## Current Baseline

Animus is a Rust-only agent orchestrator with:

- the current Cargo workspace members under `crates/`, as defined in `Cargo.toml`
- the CLI binary named `animus`
- a visible CLI surface that includes `project` and `queue`
- scoped runtime state under `~/.animus/<repo-scope>/`
- project-local workflow YAML overlays under `.animus/workflows.yaml` or `.animus/workflows/*.yaml`
- the web UI now ships as the standalone `launchapp-dev/animus-web-ui` plugin (paired with `animus-transport-http` + `animus-transport-graphql`); the old in-tree web stack is no longer part of the current Cargo workspace
- a stdio plugin host (`orchestrator-plugin-host`) for subject, provider, transport, and trigger plugins, plus a typed `HostError::ConnectionLost` + `classify(&HostError) -> RetryDecision` API for supervised restart decisions
- a daemon-side workflow event broadcaster that emits `phase_started` / `phase_completed` / `workflow_completed` / `workflow_failed` on the `workflow/events` ControlClient subscription (animus-protocol v0.1.10)
- an event-driven daemon scheduler: the main loop wakes on `daemon/nudge` control messages (sent fire-and-forget by `animus subject create/update/status` and `animus queue enqueue/release`), workflow/phase completion events, config hot-reloads, and precise cron deadlines computed from compiled `schedules:`; `interval_secs` is only a fallback heartbeat that bounds out-of-band pickup and paces housekeeping (heavy reconciliation legs run at most once per heartbeat period). See `docs/reference/configuration.md#scheduler-wake-model`.
- the v0.4.x plugin extraction **complete**: 18 standalone repositories live at <https://github.com/launchapp-dev> covering protocol, providers (claude / codex / gemini / opencode / oai), subject backends (default / requirements / linear / sqlite / markdown), transports (http / graphql), web UI, triggers (webhook / slack), log storage, the conformance testkit, the release-automation scripts, and the plugin template.
- the v0.4.x **pack** extraction is complete: `animus.core-skills`, `animus.task`, `animus.requirement`, and `animus.review` live as `launchapp-dev/animus-pack-{core-skills,task,requirement,review}` with v0.1.0/v0.1.1 GitHub releases. The in-tree `crates/orchestrator-config/config/bundled-packs/` directory, `pack_config/bundled.rs`, `BUILTIN_SKILL_YAMLS`, and the matching workflow-ref constants in `phase_plan.rs` were all removed. `animus init` surfaces the recommended pack list via `recommended_install` (read from `crates/orchestrator-cli/config/default-install.json`) and can install + activate those packs from their pinned GitHub release tags — offered default-yes in the interactive walkthrough, opt-in via `--install-packs` in non-interactive runs.

Do not reintroduce stale claims such as:

- stale workspace-count summaries that do not match `Cargo.toml`'s current 12 members
- "plugin extraction in flux" or "in progress" framing — extraction is complete as of v0.4.12
- `PROJECT_ROOT` or "last-project-root registry" resolution rules
- removed crates like `llm-mcp-server`, `llm-cli-wrapper`, `orchestrator-web-api`, `orchestrator-web-contracts`, `orchestrator-web-server`, `agent-runner`, `oai-runner`, or in-tree `animus-provider-{claude,codex,gemini,opencode,oai}`
- outdated CLI groups such as a top-level `planning` facade
- in-tree `inproc_subject_backend.rs` or the `InTreeTaskSubjectBackend` / `InTreeRequirementsSubjectBackend` adapters — all subject ops route through the `SubjectRouter` to installed plugins
- claims that `animus web serve` boots an in-process axum server — it now spawns installed transport + web_ui plugins

## Workspace Map

Core orchestration:

- `crates/orchestrator-cli`
- `crates/orchestrator-core` (includes the folded-in `subject_adapter` and `store` modules as of v0.5.3)
- `crates/orchestrator-config`
- `crates/protocol`

Runtime and provider layer:

- `crates/orchestrator-daemon-runtime`
- `crates/orchestrator-logging`

The standalone `agent-runner` sidecar was **deleted in v0.5.3**. The
CLI's `animus agent {run, status, cancel, control}` family is now a
thin client over `SessionBackendResolver` (see
`crates/orchestrator-cli/src/services/runtime/runtime_agent/provider_client.rs`)
— provider plugins handle CLI invocation end to end, so there is no
Unix-socket bridge, no shared-secret handshake, and no per-process
sidecar to supervise. `DaemonHealth.provider_plugins_healthy` replaces
the old `runner_connected` field; the legacy `runner_pid` /
`runner_connected` fields are kept on the wire as deprecated and always
serialize as `None` / `false`. The `animus-agent-runner-protocol` crate
on `launchapp-dev/animus-protocol` is bumped to v0.1.1 deprecated.

Plugin host + protocol:

- `crates/orchestrator-plugin-host`
- `crates/animus-plugin-protocol`
- `crates/animus-plugin-runtime`

Web surface ships out-of-tree as the `launchapp-dev/animus-web-ui` plugin together
with `animus-transport-http` and `animus-transport-graphql`. Install with
`animus plugin install-defaults --include-transports`. The legacy in-repo
`crates/orchestrator-web-server/`, `crates/agent-runner/`, and
`crates/oai-runner/` leftovers were deleted.

## Root Resolution And State

Project root resolution is currently:

1. `--project-root`
2. git common root for the current cwd or linked worktree
3. current working directory

Do not document environment-variable fallbacks unless you add them in code.

State layout is split:

- Project-local `.animus/` stores repo config and workflow YAML overlays.
- Scoped runtime state lives in `~/.animus/<repo-scope>/`.
- Global config lives in `protocol::Config::global_config_dir()` and can be overridden with `ANIMUS_CONFIG_DIR`.

Important current paths:

- Project-local config: `.animus/config.json` — deserializes `protocol::Config` (self-update `auto_update` block plus optional CLI settings like `default_subject_kind` and `mcp_servers`); it holds NO daemon runtime settings
- Daemon runtime settings: `~/.animus/<repo-scope>/daemon/pm-config.json` (scoped, not project-local; written by `animus daemon config` and hot-reloaded by the running daemon each tick)
- Workflow YAML overlays: `.animus/workflows.yaml`, `.animus/workflows/*.yaml`
- Scoped runtime root: `~/.animus/<repo-scope>/`
- Compiled workflow config: `~/.animus/<repo-scope>/config/workflow-config.v2.json`
- Agent runtime config: `~/.animus/<repo-scope>/config/agent-runtime-config.v2.json`
- State machines: `~/.animus/<repo-scope>/config/state-machines.v1.json`
- Runs: `~/.animus/<repo-scope>/runs/`
- Artifacts: `~/.animus/<repo-scope>/artifacts/`

Legacy readers still probe older repo-local run/artifact paths. Preserve compatibility when needed,
but write new features against the scoped runtime root.

## Working Rules

- Keep the repo Rust-only. Do not add `tauri`, `wry`, `tao`, `gtk`, `webkit*`, `webview*`, or similar desktop-shell dependencies.
- Treat Animus JSON state as tool-managed. Use CLI commands instead of hand-editing `.animus/*.json` or scoped state JSON.
- Supported hand-edit exception: workflow YAML overlays in `.animus/workflows.yaml` and `.animus/workflows/*.yaml`.
- In scripts, CI snippets, and automation, pass `--project-root "$(pwd)"`.
- If you change CLI behavior, update `docs/reference/cli/index.md`.
- If you change MCP tools, update `docs/reference/mcp-tools.md` and `docs/guides/agents.md`.
- Everything is `animus`. New MCP tools are named `animus.<group>.<verb>`, env vars are `ANIMUS_*`, state paths are `.animus/` or `~/.animus/<repo-scope>/`, pack ids are `animus.*`. The CLI is invoked via `animus`. The legacy `ao.*` surfaces were dropped in v0.4.0 (no aliases). See [docs/architecture/naming-contract.md](docs/architecture/naming-contract.md).
- Workflow YAML supports `${VAR}` env-var interpolation for non-secret config (URLs, team IDs, feature flags), with `${VAR:-default}` and `${VAR:?error}` fallback shapes; substitution happens before YAML parsing, and unset required vars fail with file path + line number. API keys and other credentials belong in the OS keychain via `animus secret set <KEY>` (preferred, v0.5.8+) — failing that, the daemon's process environment (legacy). The `${VAR}` interpolator and the plugin-spawn path both check keychain entries for the current `repo-scope` when the env var is unset, with explicit parent-process env winning on collision. See `docs/reference/secrets.md` and `docs/reference/configuration.md#secrets-vs-non-secret-config`.
- Workflow YAML also carries three daemon-affecting top-level blocks: `schedules:` (cron-driven workflow dispatch), `triggers:` (file watchers, webhooks, GitHub webhooks, trigger plugins), and `daemon:`. All three compile from `crates/orchestrator-config/src/workflow_config/types.rs` (`WorkflowSchedule`, `WorkflowTrigger`, `DaemonConfig`). The `daemon:` block today only consumes `auto_run_ready`, `active_hours`, `phase_routing`, and `mcp` from YAML; `pool_size`/`interval_secs` round-trip but are read from `~/.animus/<repo-scope>/daemon/pm-config.json` or CLI flags instead, and `max_task_retries`/`retry_cooldown_secs` are currently no-ops. The daemon git/merge policy keys (`auto_merge`, `auto_pr`, `auto_commit_before_merge`, `auto_prune_worktrees`) and their CLI/MCP flags were deleted in v0.5.x — merge/PR behavior lives in workflow `post_success.merge`, executed by the workflow runner plugin (the in-kernel `GitProvider`/`BuiltinGitProvider` trait and `FileServiceHub::git_provider()` were deleted in the same change). Authoring reference: `docs/reference/workflow-yaml.md#schedules`, `#triggers`, `#daemon`.
- Plugin kill-switches: `ANIMUS_DAEMON_DISABLE_TRIGGERS=1` skips the trigger plugin supervisor on daemon start (and interrupts in-progress restart backoff); `ANIMUS_DAEMON_DISABLE_SUBJECT_PLUGINS=1` skips subject plugin discovery entirely. `ANIMUS_PROVIDER_DISABLE_PLUGIN` was removed in v0.4.12 — there is no in-tree provider fallback anymore; `SessionBackendResolver` now hard-errors with an actionable install command when a requested provider plugin is missing. Both active kill-switches require a daemon restart to take effect. Documented in `docs/reference/configuration.md#plugin-kill-switches`.
- Plugin preflight: as of v0.4.12 the daemon refuses to start when required-role plugins are missing. As of the v0.5 P2 Wave C fold-in, `workflow_runner` and `queue` join the required-role set alongside `at_least_one_provider`, `subject_kind:task`, and `subject_kind:requirement`. The v0.5 reference impls are `launchapp-dev/animus-workflow-runner-default` (v0.4.0+) and `launchapp-dev/animus-queue-default` (v0.2.0+). Use `animus plugin install-defaults` ahead of time or pass `animus daemon start --auto-install` to install recommended defaults on the fly. `--skip-preflight` is the dev escape hatch (no in-tree fallback runs in production; the daemon will fail at the first plugin RPC if the plugin really is missing). Run `animus daemon preflight` for a standalone report. The in-tree workflow runner BINARY was deleted in the v0.5.1 round-4 fold-in and the `workflow-runner-v2` LIB CRATE was deleted in the v0.5.1 round-5 fold-in — its modules now live out-of-tree as `launchapp-dev/animus-runtime-shared` v0.1.0 (path-deped locally until that repo is published). The daemon scheduler resolves the workflow_runner binary via kind-based plugin discovery (`orchestrator_plugin_host::discover_by_kind("workflow_runner")`) with binary-name fallback for back-compat.
- Prefer narrow verification over full-workspace rebuilds while iterating.

## Implementation Landmarks

CLI and dispatch:

- `crates/orchestrator-cli/src/main.rs`
- `crates/orchestrator-cli/src/cli_types/root_types.rs`
- `crates/orchestrator-cli/src/cli_types/`
- `crates/orchestrator-cli/src/shared/output.rs`

Core services and state:

- `crates/orchestrator-core/src/config.rs`
- `crates/orchestrator-core/src/services.rs`
- `crates/orchestrator-core/src/services/`
- `crates/orchestrator-core/src/workflow/`

Workflow and runtime config:

- `crates/orchestrator-config/src/workflow_config/`
- `crates/orchestrator-config/src/agent_runtime_config.rs`

Plugin host + preflight:

- `crates/orchestrator-plugin-host/src/`
- `crates/orchestrator-core/src/services/plugin_preflight.rs`
- `crates/orchestrator-cli/src/services/operations/ops_plugin.rs`
- `crates/orchestrator-daemon-runtime/src/control/`

Web UI:

- Out-of-tree at `launchapp-dev/animus-web-ui` (plus `animus-transport-http` / `animus-transport-graphql`). The legacy `crates/orchestrator-web-server/` directory was deleted.

## CLI Reality Check

Visible top-level command groups currently include:

- `daemon` (with `start`, `run`, `stop`, `status`, `preflight`, ...)
- `agent`, `project`, `queue`, `workflow`, `subject`
- `history`, `git`, `approval`, `skill`
- `status`, `output`, `mcp`, `web`, `init`, `doctor`
- `pack`, `plugin` (with `install`, `install-defaults`, `list`, `info`, ...), `trigger`, `logs`, `flavor`

Hidden but implemented: `animus git confirm {request, respond, outcome}` — a
hidden back-compat alias for the top-level `animus approval` group (same
handlers, same project-local git-confirmations store).

`animus web serve` does not boot an in-process HTTP server as of v0.4.12.
It discovers installed `transport_backend` + `web_ui` plugins, spawns
them, then opens the browser. Run
`animus plugin install-defaults --include-transports` to install the
standard set (`launchapp-dev/animus-transport-http`,
`animus-transport-graphql`, `animus-web-ui`).

`animus daemon start` always starts the daemon as a detached background
process as of v0.6 (the legacy `--autonomous` flag is a hidden, deprecated
no-op kept for fleet automation back-compat). `animus daemon run` is the
foreground dev/debug verb. The pre-v0.6 behavior where flag-less
`daemon start` fell through to the foreground run path was removed.

`animus daemon start` and `animus daemon run` perform a plugin preflight
before booting. Default posture is refuse-to-start when any required role
is unsatisfied; the error surfaces the exact `animus plugin install ...`
command to fix it. Flags: `--auto-install` installs recommended defaults
from `launchapp-dev` releases, `--skip-preflight` bypasses the check.
`animus daemon preflight` runs the same check as a standalone report.

Tasks and requirements live exclusively under the unified
`animus subject --kind <kind>` surface as of v0.4.4. The legacy
`animus task` and `animus requirements` command trees were removed.
As of v0.4.12 the in-tree `InTreeTaskSubjectBackend` and
`InTreeRequirementsSubjectBackend` adapters were deleted as well —
all subject ops route through the `SubjectRouter` to installed
`subject_backend` plugins. Install
`launchapp-dev/animus-subject-default` (kind=task) and
`launchapp-dev/animus-subject-requirements` (kind=requirement) via
`animus plugin install-defaults --include-subjects` to keep the
`kind=task` and `kind=requirement` surfaces routable. Use
`ANIMUS_DAEMON_DISABLE_SUBJECT_PLUGINS=1` to skip subject discovery
entirely.

Removed in v0.5.x: the `animus model` group (use `animus doctor --check
cli_tools` / `--check api_keys` and the agent-runtime config files) and
the `animus runner` group (`runner health` was absorbed into
`animus plugin status` as the `providers` array +
`provider_plugins_healthy` aggregate; `runner orphans {detect,cleanup}`
became the doctor's `orphan_cli_processes` check — `animus doctor --fix`
prunes stale cli-tracker entries, live PIDs get a manual `kill`
suggestion). The matching `animus.runner.*` MCP tools were dropped with
no aliases.

Also removed in v0.4.4: `animus cloud`, `animus setup`, `animus now`,
and `animus errors`. Use `animus init` (onboarding), `animus status`
(unified inbox + status), and `animus history` (operational error
history) instead. The matching MCP tool families (`ao_task_*`,
`ao_requirements_*`, `ao_cloud_*`, `ao_errors_*`) were dropped at the
same time.

Subject CLI verbs available against any registered backend:

- `animus subject list --kind <kind>`
- `animus subject get --kind <kind> --id <id>`
- `animus subject create --kind <kind> --title <title> [...]`
- `animus subject update --kind <kind> --id <id> [...]`
- `animus subject next --kind <kind>`              (highest-priority Ready)
- `animus subject status --kind <kind> --id <id> --status <s>`

Use `cargo run -p orchestrator-cli -- --help` or `docs/reference/cli/index.md`
when changing or documenting the command tree.

## Service Model

The main production hub is `FileServiceHub`. Tests commonly use `InMemoryServiceHub`.
If you touch orchestration behavior, look for both implementations and update tests accordingly.

Keep these patterns intact:

- CLI output uses the `animus.cli.v1` envelope for `--json`
- state mutations flow through service APIs, not ad hoc file writes
- workflow YAML overlays compile into generated runtime config under scoped state
- git/worktree behavior is repo-scope aware

## Sub-agent Codex Self-Vet (required before commit)

Every sub-agent dispatched to write or modify code in this repo MUST run
`codex review --uncommitted` on its staged changes BEFORE committing. This
catches issues at the sub-agent layer instead of waiting for main-session
review iterations.

Procedure inside the sub-agent:

1. Pass all standard gates (cargo check, tests, fmt, clippy).
2. `git add -A` to stage changes.
3. Run:

   ```bash
   source ~/.claude/skills/gstack/bin/gstack-codex-probe 2>/dev/null
   timeout 540 codex review --uncommitted \
     -c 'model_reasoning_effort="high"' \
     --enable web_search_cached \
     < /dev/null
   ```

4. If codex returns any `[P1]` findings: fix them, re-verify, re-run codex.
   Repeat until codex returns no `[P1]`.
5. If codex returns `[P2]` findings:
   - Trivially fixable (~10 LOC): fix inline.
   - Larger: leave `// TODO(codex-p2):` comment + flag in final report.
6. Commit only after `[P1]` clear.
7. Document in the agent's final report: rounds run, findings per round,
   what was fixed inline vs deferred.

Reason: independent cross-model review catches blind spots a single
model misses. Time cost (~3-5 min per round) is paid back many times
over by reducing main-session review iterations.

## Build And Test Commands

Rust:

```bash
cargo animus-fmt
cargo animus-lint
cargo animus-bin-check
cargo test -p orchestrator-cli
cargo test --workspace
```

Prefer targeted crate or package tests while iterating. Use workspace-wide checks when the change
crosses crate boundaries or touches shared contracts.

Web UI sources moved out-of-tree to
`launchapp-dev/animus-web-ui` in v0.4.12 — run the web build commands
in that repo, not here. If GraphQL contracts change, propagate to the
upstream `animus-transport-graphql` plugin repo and bump its pin.

## Animus-Managed Workflow

Animus is meant to self-host its planning and execution state.

Common flow via the unified subject surface (the only surface as of
v0.4.4 — `animus task` and `animus requirements` are gone):

```bash
animus subject next --kind task
animus subject status --kind task --id task:TASK-XXX --status in-progress
animus workflow run --task-id TASK-XXX
animus queue list
animus daemon health
```

If a task is specifically about persistence or migrations, it can justify direct state-file work.
Otherwise, treat Animus state as a command surface, not a manual editing target.
