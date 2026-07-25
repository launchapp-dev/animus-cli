# MCP Tools Reference

All MCP tools exposed by `animus mcp serve`. The current top-level server
registers 97 built-in tools across daemon, cost, queue, agent, output,
workflow, plugin, skill, subject, logs, tool-discovery, and top-level memory families. These
tools allow AI agents to interact with the Animus orchestrator over the Model
Context Protocol. Each tool wraps an `animus` CLI command, accepting JSON input
and returning structured results.

That headline 97 counts the full management-mode surface. A default
agent-injected server (`animus mcp serve` without `--management`) exposes 95:
the two `animus.interactions.*` management tools are gated behind
`--management` so an agent can never list or answer its own pending approvals.

The same server also exposes 6 built-in resources: 3 current `animus://`
resources plus 3 legacy `ao://` aliases retained for v0.3 back-compat.

Most project-scoped tools accept an optional `project_root` parameter to override
the server default. Marketplace tools may omit `project_root` because they
operate on the public registry. Plugin mutation tools that touch installed
binaries can still accept `project_root` so project-local `.animus/plugins.lock`
participates in integrity tracking when present.

When a trusted host starts the server with `--require-actor --actor-json`,
Animus pins that actor for the server lifetime. Missing or malformed identity
stops startup, child commands cannot replace the pin, and the server removes
every route whose command/protocol cannot enforce it. The retained surface is
workflow run/list/get, workflow config get/validate, workflow output reads, chat
send, subject list/get/create/update/batch-create/batch-update/status,
interaction creation/management, and tool discovery. Subject `next`, queue
operations, config writes, resources, and other global routes are deliberately
unavailable rather than silently downgraded. Actor-bound subject calls use the
v2 subject protocol and never fall back to legacy v1 methods. See
[Actor-bound application contract](../architecture/actor-bound-application-contract.md).

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

## Agent Control (12 tools)

| Tool | Description | Key Parameters |
|---|---|---|
| `animus.agent.list` | List configured project agent profiles | `project_root` |
| `animus.agent.get` | Get a configured agent profile | `id`, `project_root` |
| `animus.agent.run` | Launch an AI agent to execute work | `tool`, `model`, `prompt`, `cwd`, `timeout_secs`, `context_json`, `runtime_contract_json`, `detach`, `run_id`, `project_root` |
| `animus.agent.control` | Control a running agent (pause/resume/terminate) | `run_id`, `action` (`pause`, `resume`, `terminate`) |
| `animus.agent.status` | Get status of an agent run | `run_id` |
| `animus.agent.memory.get` | Read project-scoped agent memory | `agent`, `project_root` |
| `animus.agent.memory.append` | Append project-scoped agent memory | `agent`, `text`, `source`, `project_root` |
| `animus.agent.memory.clear` | Clear project-scoped agent memory | `agent`, `project_root` |
| `animus.agent.message.send` | Send a message on a configured agent channel | `channel`, `from`, `to`, `text`, `workflow_id`, `phase_id`, `project_root` |
| `animus.agent.message.list` | List project-scoped agent messages | `channel`, `agent`, `limit`, `project_root` |
| `animus.agent.ask` | Ask a human one or more questions and wait for the answer. Two forms: (1) flat single question — `question` + optional `options[]`, returns `{ id, answer, answered_by }`; (2) structured `questions[]` (multi-question / multi-select / described options — gives codex/gemini/opencode parity with claude's native AskUserQuestion channel), each entry `{ question, header?, options:[{ label, description? }], multi_select? }`, returns `{ id, answers: { <question>: <label \| [labels] \| text> }, response?, answer }` where `answer` is a readable join for back-compat. Block mode parks until answered or timed out (structured timeout error tells the agent to proceed with its best judgment); suspend mode returns `{ status: "pending", interaction_id, instruction }` immediately and pauses the bound workflow | `agent_id`, `question`, `options[]`, `questions[]`, `timeout_secs` (default 600, max 3600), `workflow_id`, `task_id`, `wait` (`block` \| `suspend`) |
| `animus.agent.request_approval` | Request human approval for a sensitive action and wait for the decision; the agent profile's `approval_policy` can auto-allow/auto-deny without escalating (`default: allow` approves everything, `deny` rejects, `llm` auto-approves via a judge model that reads the tool call and returns allow/deny recorded with `source: "llm"`, falling back to manual escalation on any evaluator failure), and a block-mode timeout denies (fail closed). Doubles as the claude CLI's `--permission-prompt-tool`: invoked with `{ tool_name, input, tool_use_id }` it answers with the SDK permission payload in the result text — `{ behavior: "allow", updatedInput: <original or modified input>, updatedPermissions? }` or `{ behavior: "deny", message }` — with the legacy `{ tool, result: { decision, source, … } }` envelope alongside. `tool_name: "AskUserQuestion"` becomes a structured Question record (bypasses the approval policy) whose allow answer carries `updatedInput { questions, answers, response? }`. Suspend mode pauses the bound workflow: voluntary calls get the pending payload; native prompt-tool calls get `behavior: "deny"` with the end-your-turn instruction (the session resumes with the answer as feedback) | `agent_id`, `action` (derived from `tool_name` when omitted), `tool_name`, `input` \| `arguments`, `tool_use_id`, `suggestions`, `timeout_secs` (default 600, max 3600), `workflow_id`, `task_id`, `wait` (`block` \| `suspend`) |

Unlike most tools on this server, the two blocking escalation tools accept no
`project_root` override — they always operate on the server's own project
scope so a payload cannot route an approval through another project's (more
permissive) `approval_policy`. The agent identity can be pinned too: the
`animus mcp serve --agent-id <ID>` flag (appended automatically by the
ad-hoc `animus agent run --agent` / `animus chat send --agent` injection
path) or the `ANIMUS_MCP_AGENT_ID` env var on the server process make the
payload `agent_id` ignored, so an agent cannot claim a sibling profile with
a looser policy. Without a pin, the payload `agent_id` selects the policy
profile — pin the identity wherever the host knows it.

The workflow context can be pinned the same way: `animus mcp serve
--workflow-id <ID>` (or `ANIMUS_MCP_WORKFLOW_ID` on the server process)
binds escalations to that workflow and overrides the payload `workflow_id`.
A workflow pin also flips the default `wait` mode from `block` to `suspend`:
the tool records the pending interaction, pauses the workflow via the
service API, stamps the interaction id into the phase session checkpoint,
and returns immediately with an `instruction` telling the agent to summarize
its in-progress state and end the turn. The payload may downgrade
suspend→block; a block→suspend request on an unpinned server is ignored
with a warning (there is no workflow to resume). Answering the interaction
resumes the suspended workflow with the decision as feedback (see
`animus.interactions.answer` below).

---

## Interactions (2 tools)

Non-blocking management surface over the pending-interaction store under
`~/.animus/<repo-scope>/interactions/` — the same store the blocking
`animus.agent.ask` / `animus.agent.request_approval` calls park on. The CLI
equivalent is `animus agent interactions {list, show, answer}`.

These two tools are only registered when the server is started with
`animus mcp serve --management`. The default (agent-injected) server omits
them so an agent can never list or answer its own pending approvals — the
approval gate stays human-only.

| Tool | Description | Key Parameters |
|---|---|---|
| `animus.interactions.list` | List pending interactions (set `all` to include answered/expired) | `all`, `agent_id`, `limit`, `project_root` |
| `animus.interactions.answer` | Answer a question with `text`, a structured (AskUserQuestion) question with `answers` keyed by exact question text (string, or array of labels for multi-select) and/or a freeform `response`, or an approval with `decision` (`allow`/`deny`) plus optional `message`; unblocks the parked agent. Allowed approvals may carry `updated_input` (echoed as `updatedInput`), explicit `updated_permissions`, or `remember: true` (echoes the record's localSettings-destination suggestions as `updatedPermissions`). When the record was created by a suspend-mode escalation (it carries the pinned `workflow_id`) and that workflow is suspended, the answer triggers the detached-runner resume with the decision as feedback; a resume failure never fails the answer and the response carries a `workflow_resume.guidance` command (`animus workflow resume <id>`) instead | `id`, `text`, `decision`, `message`, `answers`, `response`, `updated_input`, `updated_permissions`, `remember`, `answered_by`, `project_root` |

---

## Daemon Management (12 tools)

| Tool | Description | Key Parameters |
|---|---|---|
| `animus.daemon.start` | Start the Animus daemon for task scheduling and agent management | `pool_size` (alias: `max_agents`), `interval_secs`, `startup_cleanup`, `resume_interrupted`, `reconcile_stale`, `stale_threshold_hours`, `max_tasks_per_tick`, `phase_timeout_secs`, `auto_install`, `skip_preflight`, `project_root` |
| `animus.daemon.stop` | Stop the daemon gracefully | `project_root` |
| `animus.daemon.status` | Check if daemon is running and view basic state | `project_root` |
| `animus.daemon.health` | Get detailed health metrics (active agents, queue, capacity); payload carries an additive `healthy` boolean verdict (false when the daemon is down/crashed or any plugin is unhealthy; a paused runtime stays true) | `project_root` |
| `animus.daemon.pause` | Pause the scheduler without stopping the daemon | `project_root` |
| `animus.daemon.resume` | Resume the scheduler after a pause | `project_root` |
| `animus.daemon.events` | List recent daemon events for debugging and monitoring | `limit`, `project_root` |
| `animus.daemon.agents` | List currently running agent tasks and their status | `project_root` |
| `animus.daemon.logs` | Read recent daemon log entries | `limit`, `search`, `project_root` |
| `animus.daemon.config` | Read current daemon automation settings | `project_root` |
| `animus.daemon.config-set` | Update daemon runtime settings and notification config | `pool_size` (alias: `max_agents`), `interval_secs`, `max_tasks_per_tick`, `stale_threshold_hours`, `phase_timeout_secs`, `max_daily_usd`, `notification_config_json`, `notification_config_file`, `clear_notification_config`, `project_root` |
| `animus.daemon.observe` | Routing front-door over the existing observability surfaces: returns the merged, chronological window of daemon events + logs (or routes to a single `source`). Non-streaming — always returns and never follows live. Works offline (reads scoped event/log history; the daemon need not be running) | `since`, `source` (`logs`/`events`/`stream`/`workflow`), `workflow_id`, `limit`, `project_root` |

---

## Cost & Budget (3 tools)

| Tool | Description | Key Parameters |
|---|---|---|
| `animus.cost.decisions` | List recorded budget-cap breaches from the scoped breach log. Works offline (reads the scoped breach log; the daemon need not be running) | `since`, `project_root` |
| `animus.budget.get` | Read the fleet budget posture: fleet daily spend cap, today's rolling-24h spend, remaining headroom, whether the cap is exceeded, whether dispatch is paused on the cap, and every configured per-workflow / per-phase budget cap. Works offline | `project_root` |
| `animus.budget.set` | Set or clear the fleet daily spend cap (admin). Wraps `daemon config --max-daily-usd`; hot-reloaded by the running daemon | `max_daily_usd`, `clear`, `project_root` |

---

## Subject Operations (8 tools)

The subject surface replaces the per-domain `animus.task.*` and
`animus.requirements.*` tool families removed in v0.4.4. Set `kind` to `task`,
`requirement`, or any other kind claimed by an installed `subject_backend`
plugin (e.g. `linear`, `jira`, `github-issue`).

| Tool | Description | Key Parameters |
|---|---|---|
| `animus.subject.list` | List subjects for a kind via the active `subject_backend` plugin. Returns a bounded page by default (`limit` defaults to 50; pass `limit: 0` to remove the cap); the result carries `next_cursor` (and `total` when the backend reports it) — pass `cursor` to fetch the next page. | `kind`, `status`, `limit`, `cursor`, `project_root` |
| `animus.subject.get` | Fetch a subject by wire id (`<kind>:<native_id>`) | `kind`, `id`, `project_root` |
| `animus.subject.create` | Create a subject through the active `subject_backend` plugin. `data` is a JSON object of structured custom fields (declared kind fields with no dedicated arg, e.g. a transcript's `source` / `occurred_at`) merged into the subject's `data`. | `kind`, `title`, `priority`, `status`, `labels[]`, `body`, `data`, `project_root` |
| `animus.subject.update` | Update a subject through the active `subject_backend` plugin. `data` is a JSON object of structured custom fields merged into the subject's `data` (via `patch.custom`). | `kind`, `id`, `title`, `priority`, `status`, `labels[]`, `body`, `data`, `project_root` |
| `animus.subject.next` | Return the highest-priority Ready subject for the given kind | `kind`, `project_root` |
| `animus.subject.status` | Set the status of a subject by id through the active `subject_backend` | `kind`, `id`, `status`, `project_root` |
| `animus.subject.batch-create` | Create up to 100 subjects of one kind in a single call (per-item dispatch) | `kind`, `items[]` (`title`, `status`, `priority`, `labels[]`, `body`, `data`), `on_error`, `project_root` |
| `animus.subject.batch-update` | Patch up to 100 subjects of one kind in a single call (per-item dispatch). A per-item `data` object (→ `patch.custom`) makes a data-only item valid. | `kind`, `items[]` (`id`, `status`, `priority`, `labels[]`, `data`), `on_error`, `project_root` |

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

## Environment Operations (4 tools)

Surface the CLI's `animus environment {list,get,teardown,reap}` node-management
verbs to MCP callers. They drive the installed `environment` plugin's node-
management surface (`environment/list` · `/get` · `/teardown_node` · `/reap`) so
agents + the portal can inspect and reap ephemeral run nodes without a dashboard
delete. `reap` defaults to deleting only DEAD (FAILED/CRASHED) nodes — always
safe, since a live node is never dead; `all`+`force` also reaps healthy orphans.

| Tool | Description | Key Parameters |
|---|---|---|
| `animus.environment.list` | List managed environment nodes with their state + orphan flag | `environment`, `project_root` |
| `animus.environment.get` | Describe one managed node by substrate id or name | `id`, `environment`, `project_root` |
| `animus.environment.teardown` | Destroy one managed node by substrate id or name (idempotent) | `id`, `environment`, `project_root` |
| `animus.environment.reap` | Reap orphaned/dead nodes (default: only dead) | `all`, `force`, `dry_run`, `older_than_secs`, `environment`, `project_root` |

---

## Workflow Operations (22 tools)

### Runtime & Inspection Tools (12)

| Tool | Description | Key Parameters |
|---|---|---|
| `animus.workflow.run` | Start a workflow for a subject (async, via daemon). Pass `subject_id` for any kind — qualified `task:TASK-001` / `blog:BLOG-001` (kind trusted) or bare `TASK-001` (kind resolved by the router) | `title`, `subject_id`, `description`, `workflow_ref`, `input_json`, `project_root` |
| `animus.workflow.run-multiple` | Batch-run workflows for multiple subjects | `runs[]` (each: `subject_id`, `workflow_ref`, `input_json`), `on_error`, `project_root` |
| `animus.workflow.execute` | Execute a workflow synchronously (no daemon) | `subject_id`, `workflow_ref`, `phase`, `model`, `tool`, `phase_timeout_secs`, `input_json`, `project_root` |
| `animus.workflow.get` | Get full workflow state by ID | `id`, `project_root` |
| `animus.workflow.list` | List workflow executions | `status`, `workflow_ref`, `subject_id`, `phase_id`, `search`, `sort`, `limit`, `offset`, `max_tokens`, `project_root` |
| `animus.workflow.pause` | Pause a running workflow | `id`, `confirm`, `dry_run`, `project_root` |
| `animus.workflow.cancel` | Cancel a running workflow permanently | `id`, `confirm`, `dry_run`, `project_root` |
| `animus.workflow.resume` | Resume a paused workflow | `id`, `project_root` |
| `animus.workflow.decisions` | List decisions made during workflow execution | `id`, `limit`, `offset`, `max_tokens`, `project_root` |
| `animus.workflow.checkpoints.list` | List saved workflow state checkpoints | `id`, `limit`, `offset`, `max_tokens`, `project_root` |
| `animus.workflow.phase.approve` | Approve a gated workflow phase | `workflow_id`, `phase_id` (alias: `phase`), `feedback` (alias: `note`), `project_root` |
| `animus.workflow.phase.reject` | Reject a gated workflow phase (mirror of approve, on the decline path). Requires a pending gate phase; `reason` (the rejection note) is required | `workflow_id`, `phase_id` (alias: `phase`), `reason` (alias: `note`/`feedback`), `project_root` |

### Definition Tools (5)

| Tool | Description | Key Parameters |
|---|---|---|
| `animus.workflow.phases.list` | List workflow phase definitions | `project_root` |
| `animus.workflow.phases.get` | Get a workflow phase definition | `phase`, `project_root` |
| `animus.workflow.definitions.list` | List workflow definitions | `project_root` |
| `animus.workflow.config.get` | Read effective workflow configuration | `project_root` |
| `animus.workflow.config.validate` | Validate workflow config for shape errors and broken references | `project_root` |
| `animus.workflow.config.set` | Replace the full config via the writable config_source plugin (validates first; rejected on read-only sources) | `file`, `project_root` |
| `animus.workflow.config.agent-set` | Create or replace one agent definition (read-modify-write the full config) | `id`, `input_json`, `project_root` |
| `animus.workflow.config.agent-remove` | Remove one agent definition | `id`, `project_root` |
| `animus.workflow.config.workflow-set` | Create or replace one workflow definition (read-modify-write) | `input_json`, `project_root` |
| `animus.workflow.config.workflow-remove` | Remove one workflow definition | `id`, `project_root` |

---

## Queue Operations (7 tools)

| Tool | Description | Key Parameters |
|---|---|---|
| `animus.queue.list` | List queued subject dispatches | `project_root` |
| `animus.queue.stats` | Get aggregate queue depth and status counts | `project_root` |
| `animus.queue.enqueue` | Add a subject dispatch to the queue (immediate, or deferred via `run_at`). Pass `subject_id` for any kind — qualified `task:TASK-001` / `blog:BLOG-001` (kind trusted) or bare `TASK-001` (kind resolved by the router) | `title`, `subject_id`, `description`, `workflow_ref`, `input_json`, `run_at`, `expire_after`, `project_root` |
| `animus.queue.reorder` | Set preferred dispatch order | `subject_ids[]`, `project_root` |
| `animus.queue.hold` | Hold one or more pending subjects from dispatch | `subject_id`, `subject_ids[]`, `project_root` |
| `animus.queue.release` | Release one or more held subjects for dispatch | `subject_id`, `subject_ids[]`, `project_root` |
| `animus.queue.drop` | Remove one or more queued subject dispatches permanently | `subject_id`, `subject_ids[]`, `project_root` |

`animus.queue.enqueue` accepts `run_at` (an RFC 3339 timestamp or relative
offset like `90s` / `30m` / `2h` / `3d`) to **defer** dispatch: the entry
stays queued but is not leased until the time passes. `expire_after` (e.g.
`10m`) sets a grace window after `run_at` past which a still-pending entry is
dropped instead of dispatched late. Enqueue is never deduplicated (immediate
or deferred) — a collision with an existing entry for the same subject still
enqueues and returns a `warning` in the result for the agent to act on. This
is the path an agent uses to schedule a one-off run for a specific time. The
daemon is queue-only: it executes only enqueued work plus cron schedules and
never auto-dispatches `Ready` tasks from the subject backend.

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
| `animus.skill.get` | Resolve a skill by name and return its full `SkillDefinition` plus provenance. Resolution priority: project > user > installed/pack > agent-host. Agent-host responses include a `notice` field explaining the structural-field strip. Includes a non-fatal `warnings` array when the definition contains inert declarations (an `activation.tools` or `adapters` entry that is not a built-in tool id — `claude`, `codex`, `gemini`, `opencode`, `oai-runner` — and, unless a custom CLI tool with that exact id is configured, silently never matches) | `project_root`, `name` |
| `animus.skill.search` | Case-insensitive substring match over skill `name`, `description`, and `tags`. Returns the same row shape as `animus.skill.list` plus a `truncated` flag when matches exceed `limit` | `project_root`, `query`, `source`, `limit` (default 50) |
| `animus.skill.create` | Author a skill at project or user scope, validated by round-tripping through the loader. `scope: "project"` (default) writes `.animus/config/skill_definitions/<name>.yaml`; `scope: "user"` writes `~/.animus/config/skill_definitions/<name>.yaml` (available across projects; project shadows user on name collision). Refuses to shadow an existing skill at the same scope unless `overwrite` is set. The result carries the same non-fatal `warnings` array as `animus.skill.get` when the written definition contains inert tool-id declarations | `project_root`, `name` (slug), `scope` (`"project"` \| `"user"`, default `"project"`), `description`, `prompt`, `tags`, `tool_policy`, `model`, `mcp_servers`, `category`, `activation`, `capabilities`, `overwrite` |
| `animus.skill.update` | Patch an existing project- or user-scoped skill; only the supplied fields change, the rest are preserved. When `scope` is omitted the skill is patched at the single scope where it exists; if it exists at both scopes the call fails until an explicit `scope` is passed. Rejects unknown skills and installed/pack/agent-host sources | `project_root`, `name`, `scope` (`"project"` \| `"user"`, optional unless the name exists at both scopes), plus any of the `animus.skill.create` fields to patch |

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

### Two memory families: `animus.memory.*` vs `animus.agent.memory.*`

There are two distinct memory tool families and they are NOT interchangeable.
Pick by who you are and whose memory you are touching:

- **`animus.memory.*` (4 tools, this section)** — an **any-agent-id document
  store**. Every call takes `agent_id` explicitly, so one caller can read or
  write the memory of *any* agent. This is BOTH the family external clients
  use (dashboards, Claude Desktop, Cursor, operator scripts) AND the family a
  memory-capable workflow agent actually receives: the sidecar injected for
  `capabilities.memory: true` runs `animus mcp memory`, which exposes
  `animus.memory.*` only. It also carries `list` (with `prefix` filtering) and
  per-entry `entry_id` operations the agent family omits.
- **`animus.agent.memory.*` (3 tools, see [Agent Control](#agent-control-12-tools))** — CLI-shaped
  wrappers over `animus agent memory ...` (they validate that the agent
  profile exists), exposed **only on the full `animus` MCP server**
  (`animus mcp serve`). A workflow agent sees them only when its profile
  explicitly lists the `animus` server in `mcp_servers` — they are NOT part of
  the default injected-memory sidecar.

Rule of thumb: **inside a phase, call `animus.memory.append` with your own
`agent_id` (the phase prompt's coaching paragraph spells this out); use
`animus.agent.memory.*` only when the full `animus` server is configured;
from an operator or dashboard client, either works — `animus.memory.*` is the
richer surface.** Both write to the same on-disk store keyed by `agent_id`
under the repo-scoped runtime root, so a value appended by one family is
visible to the other.

### Retention (FIFO cap)

Memory is bounded. Each append trims the document to at most
`memory.max_entries` entries (set per agent profile in workflow YAML),
dropping the **oldest** entries first (FIFO). When a profile omits the cap, the
store applies a generous default of **200** entries so memory can never grow
without bound. `memory.max_entries: 0` is rejected by config validation. There
are no per-entry expiry timestamps — trimming is purely count-based FIFO.

### Observability

Each successful `append` or `clear` (single-entry delete or `delete_all`) emits
exactly one `agent-memory-updated` record to the daemon event log
(`animus daemon events`), carrying `{ agent_id, operation, entry_count }`.
Agent messaging emits `agent-message-sent` likewise. Read paths
(`get`/`list`) emit nothing, so watching coordination never produces an event
storm. Emission is best-effort: a failed event write never fails the memory
operation, and it works even without a running daemon (the event log is a
plain JSONL file under the global Animus state dir).

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

## Tool Discovery (2 tools)

Meta-tools over the server's own live tool registry. Agents with tight context
budgets can discover which `animus.*` tool fits an intent instead of carrying
every schema. Results always reflect the serving instance's registry — new
tools become searchable automatically, and the management-gated
`animus.interactions.*` tools only appear when serving with `--management`.

| Tool | Description | Key Parameters |
|---|---|---|
| `animus.tools.search` | Search the live tool registry by intent keywords. Keywords match tool names, descriptions, and parameter names; results are ranked (name hits outrank description hits outrank parameter hits, and an exact tool-name query always ranks first) and each match carries a compact parameter summary (name, type, required, one-line description). | `query`, `limit` (default 8, max 50) |
| `animus.tools.list` | List every registered tool grouped by family with a one-line summary each — a compact catalog with no input schemas. | (none) |

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

The batch tools — `animus.workflow.run-multiple`,
`animus.subject.batch-create`, and `animus.subject.batch-update` — dispatch
their items one at a time through the same code paths as the matching
single-item tools (no new backend protocol), and accept an `on_error`
parameter:

| Value | Behavior |
|---|---|
| `"continue"` | Process all items regardless of failures |
| `"stop"` (default) | Stop processing after the first failure; remaining items are marked `"skipped"` and never execute |

Batch responses use the `animus.mcp.batch.result.v1` schema with a summary of
succeeded/failed/skipped counts and per-item results. Failed items carry the
same structured `error` (and, when determinate, `remediation`) payload as
single-tool errors; one item failing never corrupts its neighbors' results.

Maximum batch size is 100 items per call; larger requests are rejected with a
clear `exceeds maximum` error before any item executes.

## Error Remediation

When a tool fails, the structured error payload carries the wrapped CLI
error (`error`), the process `exit_code`, and raw `stderr`. For determinate
failure classes it also carries a machine-actionable `remediation` object:

```json
{
  "tool": "animus.subject.list",
  "exit_code": 5,
  "error": { "code": "unavailable", "message": "...", "exit_code": 5 },
  "stderr": "...",
  "remediation": {
    "kind": "missing_plugin",
    "install_command": "animus plugin install-defaults --include-subjects",
    "next_step": "Install a subject_backend plugin that serves this kind, then retry."
  }
}
```

| `remediation.kind` | When | Fields |
|---|---|---|
| `missing_plugin` | A required plugin is not installed (subject backend for the kind, provider plugin for the tool, queue plugin) | `install_command` — the exact `animus plugin install ...` command; `next_step` — what to do after installing |
| `daemon_not_running` | The command needs a running daemon (`unavailable` from events/daemon surfaces) | `next_step` — always `animus daemon start` |
| `invalid_input` | The CLI rejected the arguments (exit code 2) | `help` — the CLI's hint describing the accepted input |

`missing_plugin` remediations are threaded as structured data from the CLI's
typed error constructors (`error.details.remediation` in the `animus.cli.v1`
envelope) — the install command is never scraped out of the human-readable
message. Indeterminate failures (e.g. `internal` errors) carry no
`remediation` field; absence means "no known mechanical fix".

Batch item errors (`results[].error`) carry the same `remediation` field
under the same rules.

See also: [JSON Envelope Contract](json-envelope.md), [CLI Command Surface](cli/index.md).
