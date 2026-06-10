# MCP Tools Reference

All MCP tools exposed by `animus mcp serve`. The current top-level server
registers 78 built-in tools across daemon, queue, agent, output, runner,
workflow, plugin, skill, subject, logs, and top-level memory families. These
tools allow AI agents to interact with the Animus orchestrator over the Model
Context Protocol. Each tool wraps an `animus` CLI command, accepting JSON input
and returning structured results.

The same server also exposes 6 built-in resources: 3 current `animus://`
resources plus 3 legacy `ao://` aliases retained for v0.3 back-compat.

Most project-scoped tools accept an optional `project_root` parameter to override
the server default. Marketplace tools may omit `project_root` because they
operate on the public registry. Plugin mutation tools that touch installed
binaries can still accept `project_root` so project-local `.animus/plugins.lock`
participates in integrity tracking when present.

**OAuth-protected upstream MCP servers.** Connecting *agents* to OAuth-backed
MCP servers is handled by the `animus mcp auth` CLI surface plus the
`animus-mcp-proxy` stdio bridge, not by an MCP tool. See
[`mcp-oauth.md`](mcp-oauth.md). The proxy is launched automatically for any
MCP server configured with an `oauth:` block: `authorization_code` reads from
the OS keychain, while `manual_bearer`, `client_credentials`, and
`refresh_token` resolve through the OAuth broker.

**v0.4.4 note — subject surface is now mandatory for tasks and requirements.** The legacy
`animus.task.*` / `animus.requirements.*` / `animus.cloud.*` / `animus.errors.*` MCP tool
families were removed. Use the unified `animus.subject.*` tools with `kind=task` or
`kind=requirement`; they route through installed `subject_backend` plugins, including the
default task/requirement plugins that own Animus-managed local state. External
`subject_backend` plugins (Linear, Jira, GitHub Issues, etc.) plug into the same surface and
can claim their own `kind`.

---

## MCP Resources (6 resources)

`animus mcp serve` enables MCP resources in addition to tools. The built-in
resource set is intentionally small and read-only:

| Resource URI | Description |
|---|---|
| `animus://project/tasks` | Project task index as JSON |
| `animus://project/requirements` | Project requirement index as JSON |
| `animus://project/daemon-events` | Recent daemon events as JSON; supports `?limit=N` |
| `ao://project/tasks` | Deprecated alias of `animus://project/tasks` |
| `ao://project/requirements` | Deprecated alias of `animus://project/requirements` |
| `ao://project/daemon-events` | Deprecated alias of `animus://project/daemon-events` |

The `ao://` URIs are advertised and accepted so older clients that cached the
pre-v0.4 resource names can still enumerate and read the same data.

---

## Agent Control (10 tools)

| Tool | Description | Key Parameters |
|---|---|---|
| `animus.agent.list` | List configured project agent profiles | `project_root` |
| `animus.agent.get` | Get a configured agent profile | `id`, `project_root` |
| `animus.agent.run` | Launch an AI agent to execute work | `tool`, `model`, `prompt`, `cwd`, `timeout_secs`, `context_json`, `runtime_contract_json`, `detach`, `run_id`, `runner_scope`, `project_root` |
| `animus.agent.control` | Control a running agent (pause/resume/terminate) | `run_id`, `action` (`pause`, `resume`, `terminate`), `runner_scope` |
| `animus.agent.status` | Get status of an agent run | `run_id`, `runner_scope` |
| `animus.agent.memory.get` | Read project-scoped agent memory | `agent`, `project_root` |
| `animus.agent.memory.append` | Append project-scoped agent memory | `agent`, `text`, `source`, `project_root` |
| `animus.agent.memory.clear` | Clear project-scoped agent memory | `agent`, `project_root` |
| `animus.agent.message.send` | Send a message on a configured agent channel | `channel`, `from`, `to`, `text`, `workflow_id`, `phase_id`, `project_root` |
| `animus.agent.message.list` | List project-scoped agent messages | `channel`, `agent`, `limit`, `project_root` |

---

## Daemon Management (11 tools)

| Tool | Description | Key Parameters |
|---|---|---|
| `animus.daemon.start` | Start the Animus daemon for task scheduling and agent management | `pool_size` (alias: `max_agents`), `interval_secs`, `auto_run_ready`, `auto_merge`, `auto_pr`, `auto_commit_before_merge`, `auto_prune_worktrees_after_merge`, `startup_cleanup`, `resume_interrupted`, `reconcile_stale`, `stale_threshold_hours`, `max_tasks_per_tick`, `phase_timeout_secs`, `idle_timeout_secs`, `skip_runner`, `autonomous`, `auto_install`, `skip_preflight`, `runner_scope`, `project_root` |
| `animus.daemon.stop` | Stop the daemon gracefully | `project_root` |
| `animus.daemon.status` | Check if daemon is running and view basic state | `project_root` |
| `animus.daemon.health` | Get detailed health metrics (active agents, queue, capacity) | `project_root` |
| `animus.daemon.pause` | Pause the scheduler without stopping the daemon | `project_root` |
| `animus.daemon.resume` | Resume the scheduler after a pause | `project_root` |
| `animus.daemon.events` | List recent daemon events for debugging and monitoring | `limit`, `project_root` |
| `animus.daemon.agents` | List currently running agent tasks and their status | `project_root` |
| `animus.daemon.logs` | Read recent daemon log entries | `limit`, `search`, `project_root` |
| `animus.daemon.config` | Read current daemon automation settings | `project_root` |
| `animus.daemon.config-set` | Update daemon automation settings and notification config | `auto_merge`, `auto_pr`, `auto_commit_before_merge`, `auto_prune_worktrees_after_merge`, `auto_run_ready`, `pool_size` (alias: `max_agents`), `interval_secs`, `max_tasks_per_tick`, `stale_threshold_hours`, `phase_timeout_secs`, `idle_timeout_secs`, `notification_config_json`, `notification_config_file`, `clear_notification_config`, `project_root` |

---

## Subject Operations (6 tools)

The subject surface replaces the per-domain `animus.task.*` and
`animus.requirements.*` tool families removed in v0.4.4. Set `kind` to `task`,
`requirement`, or any other kind claimed by an installed `subject_backend`
plugin (e.g. `linear`, `jira`, `github-issue`).

| Tool | Description | Key Parameters |
|---|---|---|
| `animus.subject.list` | List subjects for a kind via the active `subject_backend` plugin | `kind`, `status`, `limit`, `project_root` |
| `animus.subject.get` | Fetch a subject by wire id (`<kind>:<native_id>`) | `kind`, `id`, `project_root` |
| `animus.subject.create` | Create a subject through the active `subject_backend` plugin | `kind`, `title`, `priority`, `status`, `labels[]`, `body`, `project_root` |
| `animus.subject.update` | Update a subject through the active `subject_backend` plugin | `kind`, `id`, `priority`, `status`, `labels[]`, `project_root` |
| `animus.subject.next` | Return the highest-priority Ready subject for the given kind | `kind`, `project_root` |
| `animus.subject.status` | Set the status of a subject by id through the active `subject_backend` | `kind`, `id`, `status`, `project_root` |

---

## Log Operations (1 tool)

Surfaces the CLI's `animus logs tail` to MCP callers. Routes through the daemon
control wire when the daemon is running, otherwise reads the in-tree
`events.jsonl` fallback directly.

Unlike the CLI, the MCP surface does not expose `--follow`; this tool is a
bounded fetch for recent entries, not a live stream.

| Tool | Description | Key Parameters |
|---|---|---|
| `animus.logs.tail` | Tail recent log entries from the active `log_storage_backend` | `plugin`, `level`, `since`, `limit`, `project_root` |

---

## Workflow Operations (16 tools)

### Runtime & Inspection Tools (11)

| Tool | Description | Key Parameters |
|---|---|---|
| `animus.workflow.run` | Start a workflow for a task (async, via daemon) | `task_id`, `requirement_id`, `title`, `description`, `workflow_ref`, `input_json`, `project_root` |
| `animus.workflow.run-multiple` | Batch-run workflows for multiple tasks | `runs[]` (each: `task_id`, `workflow_ref`, `input_json`), `on_error`, `project_root` |
| `animus.workflow.execute` | Execute a workflow synchronously (no daemon) | `task_id`, `workflow_ref`, `phase`, `model`, `tool`, `phase_timeout_secs`, `input_json`, `project_root` |
| `animus.workflow.get` | Get full workflow state by ID | `id`, `project_root` |
| `animus.workflow.list` | List workflow executions | `status`, `workflow_ref`, `task_id`, `phase_id`, `search`, `sort`, `limit`, `offset`, `max_tokens`, `project_root` |
| `animus.workflow.pause` | Pause a running workflow | `id`, `confirm`, `dry_run`, `project_root` |
| `animus.workflow.cancel` | Cancel a running workflow permanently | `id`, `confirm`, `dry_run`, `project_root` |
| `animus.workflow.resume` | Resume a paused workflow | `id`, `project_root` |
| `animus.workflow.decisions` | List decisions made during workflow execution | `id`, `limit`, `offset`, `max_tokens`, `project_root` |
| `animus.workflow.checkpoints.list` | List saved workflow state checkpoints | `id`, `limit`, `offset`, `max_tokens`, `project_root` |
| `animus.workflow.phase.approve` | Approve a gated workflow phase | `workflow_id`, `phase_id` (alias: `phase`), `feedback` (alias: `note`), `project_root` |

`animus workflow phase reject` exists in the CLI, but there is currently no
matching built-in MCP tool. MCP callers can approve pending gates, but gate
rejection remains a CLI-only action.

### Definition Tools (5)

| Tool | Description | Key Parameters |
|---|---|---|
| `animus.workflow.phases.list` | List workflow phase definitions | `project_root` |
| `animus.workflow.phases.get` | Get a workflow phase definition | `phase`, `project_root` |
| `animus.workflow.definitions.list` | List workflow definitions | `project_root` |
| `animus.workflow.config.get` | Read effective workflow configuration | `project_root` |
| `animus.workflow.config.validate` | Validate workflow config for shape errors and broken references | `project_root` |

---

## Queue Operations (7 tools)

| Tool | Description | Key Parameters |
|---|---|---|
| `animus.queue.list` | List queued subject dispatches | `project_root` |
| `animus.queue.stats` | Get aggregate queue depth and status counts | `project_root` |
| `animus.queue.enqueue` | Add a subject dispatch to the queue | `task_id`, `requirement_id`, `title`, `description`, `workflow_ref`, `input_json`, `project_root` |
| `animus.queue.reorder` | Set preferred dispatch order | `subject_ids[]`, `project_root` |
| `animus.queue.hold` | Hold a pending subject from dispatch | `subject_id`, `project_root` |
| `animus.queue.release` | Release a held subject for dispatch | `subject_id`, `project_root` |
| `animus.queue.drop` | Remove a queued subject dispatch permanently | `subject_id`, `project_root` |

---

## Output & Monitoring (6 tools)

| Tool | Description | Key Parameters |
|---|---|---|
| `animus.output.run` | Get stdout/stderr from an agent execution | `run_id`, `project_root` |
| `animus.output.tail` | Get most recent output/error/thinking events | `run_id`, `task_id`, `event_types[]`, `limit`, `project_root` |
| `animus.output.monitor` | Stream real-time output from a run, optionally scoped by task or phase | `run_id`, `task_id`, `phase_id`, `project_root` |
| `animus.output.jsonl` | Get structured JSONL event log | `run_id`, `entries`, `project_root` |
| `animus.output.artifacts` | Get files generated during execution | `execution_id`, `project_root` |
| `animus.output.phase-outputs` | Get persisted workflow phase outputs | `workflow_id`, `phase_id`, `project_root` |

---

## Runner (3 tools)

| Tool | Description | Key Parameters |
|---|---|---|
| `animus.runner.health` | Check runner process health and capacity | `project_root` |
| `animus.runner.orphans-detect` | Find orphaned runner processes | `project_root` |
| `animus.runner.orphans-cleanup` | Clean up orphaned runner processes by run ID | `run_id[]`, `project_root` |

---

## Skills (5 tools)

Discover and inspect skill definitions across every source the project can see: installed packs,
registry-tracked installs, user-scoped (`~/.animus/skills/`, `~/.animus/config/skill_definitions/`),
project-scoped (`.animus/skills/`, `.animus/config/skill_definitions/`), and agent-host probes
(`~/.claude/skills/`, `~/.codex/skills/`, etc.).

Each result carries a `source` tag (`"installed"`, `"user"`, `"project"`, `"agent_host"`) plus
a `source_detail` object with provenance. For `installed` sources,
`source_detail` includes `registry`, `source`, `version`, `integrity`, and `artifact`. For
`agent_host` sources, `source_detail` includes `host` (e.g. `"claude-code"`), `scope`
(`"project"` | `"global"`), `structural_fields_stripped: true`, and `trust_tier: "prompt_text_only"`
— a reminder that structural fields (`tool_policy`, `mcp_servers`, `env`, `extra_args`,
`capabilities`, `adapters`, `codex_config_overrides`) are stripped at parse time for agent-host
skills, so only prompt text and prompt directives are trusted.

| Tool | Description | Key Parameters |
|---|---|---|
| `animus.skill.list` | Enumerate skills across all sources with optional `source` filter | `project_root`, `source` (`installed` \| `user` \| `project` \| `agent_host` \| host id like `claude-code`; `builtin` is still accepted as a backward-compatible filter but current builds do not emit builtin rows) |
| `animus.skill.get` | Resolve a skill by name and return its full `SkillDefinition` plus provenance. Resolution priority: project > user > installed/pack > agent-host. Agent-host responses include a `notice` field explaining the structural-field strip | `project_root`, `name` |
| `animus.skill.search` | Case-insensitive substring match over skill `name`, `description`, and `tags`. Returns the same row shape as `animus.skill.list` plus a `truncated` flag when matches exceed `limit` | `project_root`, `query`, `source`, `limit` (default 50) |
| `animus.skill.create` | Author a project-scoped skill, written to `.animus/config/skill_definitions/<name>.yaml` and validated by round-tripping through the loader. Refuses to shadow an existing project skill unless `overwrite` is set | `project_root`, `name` (slug), `description`, `prompt`, `tags`, `tool_policy`, `model`, `mcp_servers`, `category`, `activation`, `capabilities`, `overwrite` |
| `animus.skill.update` | Patch an existing project-scoped skill at `.animus/config/skill_definitions/<name>.yaml`; only the supplied fields change, the rest are preserved. Rejects unknown or non-project skills | `project_root`, `name`, plus any of the `animus.skill.create` fields to patch |

---

## Memory (4 tools)

Project-scoped agent memory store. Each entry is `{ id, text, created_at, source }` and lives
keyed by `agent_id` under the repo-scoped runtime root.

| Tool | Description | Key Parameters |
|---|---|---|
| `animus.memory.get` | Fetch the full memory document for an agent profile, optionally narrowing to a single entry by id | `agent_id`, `entry_id`, `project_root` |
| `animus.memory.list` | List memory entries for an agent with optional case-sensitive `prefix` filter on entry text | `agent_id`, `prefix`, `limit`, `project_root` |
| `animus.memory.append` | Add a new memory entry. The entry receives a fresh uuid and timestamp. Returns the appended entry | `agent_id`, `text`, `source`, `project_root` |
| `animus.memory.clear` | Delete a single entry by `entry_id`, or wipe all entries for the agent when `delete_all: true`. One of `entry_id` or `delete_all=true` is required | `agent_id`, `entry_id`, `delete_all`, `project_root` |

### Memory tool exposure model

The `animus.memory.*` tools are exposed in two places, with different gating:

- **Top-level `animus mcp serve`**: all four tools are always present. External MCP clients
  (Claude Desktop, Cursor, etc.) can read/write memory for any agent id.
- **Spawned workflow agents**: the memory MCP server is injected into a phase's runtime
  contract only when the active agent profile has `capabilities.memory: true`. Profiles with
  the capability absent or set to `false` do not see the memory tools in their tool list.
  See `crates/animus-runtime-shared/src/runtime_contract.rs::inject_memory_mcp_for_capable_agent`.

---

## Plugin Control (6 tools)

Discovered Animus STDIO plugins are reachable from MCP clients via these meta-tools.
Plugins themselves can declare additional `mcp_tools` in their `initialize` response;
those are aggregated automatically.

| Tool | Description | Key Parameters |
|---|---|---|
| `animus.plugin.list` | List discovered plugins (providers, subject backends, custom) with manifest metadata and discovery warnings. | `project_root`, `include_system_path` |
| `animus.plugin.info` | Spawn a plugin, complete the initialize handshake, and return manifest plus runtime capabilities. | `name`, `project_root`, `include_system_path` |
| `animus.plugin.ping` | Health-check a plugin by spawning it and sending `$/ping`. | `name`, `project_root`, `include_system_path` |
| `animus.plugin.call` | Send a JSON-RPC request to a discovered plugin. | `name`, `method`, `params`, `project_root` |
| `animus.plugin.install` | Install a plugin from `source`, local `path`, or verified `url`. MCP installs run non-interactively and auto-confirm the trusted-org prompt. | `project_root`, `source`, `path`, `url`, `tag`, `name`, `sha256`, `force`, `skip_manifest_check`, `plugin_dir`, `require_signature`, `skip_signature`, `trusted_signers`, `force_rewrite_lockfile` |
| `animus.plugin.uninstall` | Remove an installed plugin binary and unregister it. | `project_root`, `name`, `plugin_dir` |

## Plugin Marketplace (3 tools)

These tools query and update the public plugin registry view exposed by the CLI.

| Tool | Description | Key Parameters |
|---|---|---|
| `animus.plugin.search` | Search the public plugin registry. | `query`, `kind`, `tag[]`, `org`, `stability`, `registry_url`, `no_cache` |
| `animus.plugin.browse` | Browse registry entries grouped by plugin kind. | `kind`, `installed`, `available`, `registry_url`, `no_cache` |
| `animus.plugin.update` | Update one or all installed release-source plugins to the pins declared in `default-install.json`. | `name` (omit for `--all`), `tag` (only valid with `name`), `dry_run`, `force`. `registry_url` + `no_cache` are deprecated in v0.5.8 (ignored). |

Discovery order: `~/.animus/plugins.yaml` (or the legacy
`~/.config/animus/plugins.yaml` only when the new registry is absent) →
`.animus/plugins/` → global install dir (`$ANIMUS_PLUGIN_DIR` when set,
otherwise `~/.animus/plugins/`) → `$ANIMUS_PLUGIN_PATH` → `$PATH`
(`animus-provider-*` / `animus-plugin-*` prefixes; `$PATH` opt-in via
`--include-system-path`).

---

## List Tool Pagination

All list tools support pagination via these common parameters:

| Parameter | Type | Default | Max | Description |
|---|---|---|---|---|
| `limit` | integer | 25 | 200 | Maximum items to return |
| `offset` | integer | 0 | -- | Items to skip |
| `max_tokens` | integer | 3000 | 12000 | Token budget for response compaction (min: 256) |

List responses are wrapped in a guard envelope (`animus.mcp.list.result.v1`) that includes pagination metadata.

## Batch Tool Behavior

`animus.workflow.run-multiple` accepts an `on_error` parameter:

| Value | Behavior |
|---|---|
| `"continue"` | Process all items regardless of failures |
| `"stop"` | Stop processing after the first failure; remaining items are marked `"skipped"` |

Batch responses use the `animus.mcp.batch.result.v1` schema with a summary of
succeeded/failed/skipped counts and per-item results.

Maximum batch size is 100 items per call.

See also: [JSON Envelope Contract](json-envelope.md), [CLI Command Surface](cli/index.md).
