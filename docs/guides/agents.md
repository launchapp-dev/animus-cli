# Working with Animus via MCP Tools

This guide explains the current MCP surface exposed by `animus mcp serve`.
Each built-in tool maps to an `animus` CLI command and accepts JSON input.

For the full parameter table, see [MCP Tools Reference](../reference/mcp-tools.md).

## Overview

Animus currently exposes **97 built-in MCP tools** across these families:

| Group | Tools | Purpose |
|---|---:|---|
| `animus.agent.*` | 12 | Agent profiles, runs, memory, agent messaging, and blocking human-in-the-loop questions/approvals |
| `animus.daemon.*` | 12 | Daemon lifecycle, health, events, config, and the `observe` observability front-door |
| `animus.cost.*` | 1 | Budget-cap breach inspection from the scoped breach log |
| `animus.budget.*` | 2 | Read the fleet budget posture (daily cap, rolling spend, per-workflow/phase caps) and set/clear the fleet daily spend cap |
| `animus.subject.*` | 8 | Task, requirement, and external subject backends, including bulk create/update |
| `animus.workflow.*` | 22 | Workflow execution, control (incl. gate approve/reject), definition inspection, and config write-back (`config.set` + entity `agent-set`/`workflow-set`/`*-remove`) |
| `animus.queue.*` | 7 | Dispatch queue inspection and mutation |
| `animus.output.*` | 6 | Run output, artifacts, JSONL, and live monitoring |
| `animus.skill.*` | 5 | Skill discovery, inspection, and authoring at project or user scope |
| `animus.memory.*` | 4 | Project-scoped durable agent memory |
| `animus.plugin.*` | 9 | Installed-plugin inspection/mutation plus marketplace discovery/update |
| `animus.logs.*` | 1 | Tail log entries from the active log backend |
| `animus.interactions.*` | 2 | Non-blocking inbox over pending agent questions and approval requests (`animus mcp serve --management` only) |
| `animus.tools.*` | 2 | Tool discovery over the live registry: ranked keyword search plus a grouped one-line catalog |

**Tool discovery.** When you are unsure which tool fits an intent — or your
context budget is too tight to carry all 93 schemas — start with
`animus.tools.search` (e.g. `{"query": "pause workflow"}`). It searches the
server's live registry (tool names, descriptions, and parameter names), ranks
matches (name hits outrank description hits outrank parameter hits; an exact
tool-name query always ranks first), and returns each match with a compact
parameter summary so you can call it directly. `animus.tools.list` returns the
whole surface grouped by family with one-line summaries and no schemas.

Most project-scoped tools accept an optional `project_root`. Marketplace tools
may omit it because they operate on the public registry. Plugin mutation tools
such as `animus.plugin.install` and `animus.plugin.uninstall` can still take
`project_root` so project-local `.animus/plugins.lock` updates stay scoped to
the target repo when present.

The total includes both the CLI-shaped `animus.agent.memory.*` wrappers and the
top-level `animus.memory.*` document-oriented surface composed into
`animus mcp serve`.

The same server also publishes 6 built-in read-only resources: 3 current
`animus://project/*` URIs and 3 legacy `ao://project/*` aliases retained for
older clients.

## MCP Resources

Alongside tools, MCP clients can enumerate and read these built-in resources:

| Resource URI | Description |
|---|---|
| `animus://project/tasks` | Task index JSON |
| `animus://project/requirements` | Requirement index JSON |
| `animus://project/daemon-events` | Daemon event JSON (`?limit=N` supported) |
| `ao://project/tasks` | Legacy alias for `animus://project/tasks` |
| `ao://project/requirements` | Legacy alias for `animus://project/requirements` |
| `ao://project/daemon-events` | Legacy alias for `animus://project/daemon-events` |

The `ao://` aliases are still listed so clients built against older Animus
resource URIs keep working without a migration step.

## Subject Operations

`animus.subject.*` replaces the removed `animus.task.*` and
`animus.requirements.*` families. Set `kind` to `task`, `requirement`, or any
kind claimed by an installed `subject_backend` plugin.

```json
// Create a task-like subject
{
  "kind": "task",
  "title": "Add retry logic to HTTP client",
  "priority": "p1",
  "status": "ready",
  "labels": ["backend", "reliability"],
  "body": "Implement exponential backoff for 429 responses."
}
```

```json
// List ready tasks
{
  "kind": "task",
  "status": "ready",
  "limit": 10
}
```

```json
// Fetch or update a subject by backend-qualified id
{ "kind": "task", "id": "task:TASK-001" }
{ "kind": "task", "id": "task:TASK-001", "status": "in_progress" }
```

Use `animus.subject.next` to ask the active backend for the highest-priority
ready subject:

```json
{ "kind": "task" }
```

Bulk operations mirror `animus.workflow.run-multiple`: pass up to 100 items
and an `on_error` policy (`"stop"` is the default — remaining items are
skipped after the first failure; `"continue"` processes every item). Results
come back as an `animus.mcp.batch.result.v1` envelope with per-item outcomes,
so one bad item never corrupts its neighbors.

```json
// animus.subject.batch-create
{
  "kind": "task",
  "items": [
    { "title": "Fix login redirect", "priority": "p1", "labels": ["bug"] },
    { "title": "Add retry tests" }
  ],
  "on_error": "continue"
}

// animus.subject.batch-update
{
  "kind": "task",
  "items": [
    { "id": "task:TASK-001", "status": "ready" },
    { "id": "task:TASK-002", "priority": "p0" }
  ],
  "on_error": "stop"
}
```

## Workflow Operations

Use workflows to execute work for a task, a requirement, a freeform title, or
any other subject kind.

```json
// Async run via daemon
{ "task_id": "TASK-001" }

// Sync execution in-process (animus.workflow.execute)
{ "task_id": "TASK-001", "phase": "implementation", "model": "gpt-5.4" }

// Requirement-linked execution
{ "requirement_id": "REQ-001", "workflow_ref": "standard-workflow" }

// BaaS dynamic kind (blog/post/etc.): pass subject_id, NOT task_id —
// the kernel resolves the kind. Accepts qualified (blog:BLOG-001) or bare.
{ "subject_id": "blog:BLOG-001" }

// Batch-run multiple tasks (animus.workflow.run-multiple)
{
  "runs": [
    { "task_id": "TASK-001", "workflow_ref": "standard-workflow" },
    { "task_id": "TASK-002", "workflow_ref": "hotfix-workflow" }
  ],
  "on_error": "continue"
}
```

Inspection and control:

```json
{ "id": "wf-abc123" }                       // animus.workflow.get
{ "status": "running", "limit": 10 }       // animus.workflow.list
{ "id": "wf-abc123" }                       // pause / resume / cancel / decisions
{ "workflow_id": "wf-abc123", "phase_id": "po-review" } // phase.approve / phase.reject
```

Config write-back (manage agents/workflows through Animus). These persist the
config through the installed **writable** `config_source` plugin; the kernel
validates the post-pack-merge result before writing, and a read-only source
(the default `animus-config-yaml`) is rejected with an actionable error. The
`agent-set` / `workflow-set` verbs are read-modify-write on the RAW source model
and are the **definition**-management surface — distinct from the runtime
`animus.agent.*` tools. Prefer them for single-entity edits.

`config.set` takes a full RAW SOURCE model. Do NOT round-trip
`animus.workflow.config.get` into it: `config.get` returns the EFFECTIVE config
(after pack overlays are merged), so writing it back would bake pack-provided
entities into your source and shadow later pack updates. Use `config.set` only
with an externally-authored raw model; for edits, use the entity verbs.

```json
{ "file": "/tmp/config.json" }                           // animus.workflow.config.set (full model)
{ "id": "reviewer", "input_json": "{...}" }              // animus.workflow.config.agent-set
{ "id": "reviewer" }                                      // animus.workflow.config.agent-remove
{ "input_json": "{\"id\":\"ship\",\"name\":\"Ship\",\"phases\":[\"impl\"]}" } // animus.workflow.config.workflow-set
{ "id": "ship" }                                          // animus.workflow.config.workflow-remove
```

## Daemon and Queue Operations

Use the daemon tools for autonomous scheduling and queue tools for explicit
dispatch control.

```json
{}                                          // animus.daemon.status / health / agents
{ "pool_size": 3, "auto_install": true }                    // animus.daemon.start (always detaches)
{ "skip_preflight": true }                  // animus.daemon.start (dev escape hatch)
{ "pool_size": 4, "interval_secs": 10 }     // animus.daemon.config-set
{ "limit": 50 }                              // animus.daemon.events
{ "project_root": "/repo" }                  // animus.queue.list / stats
{ "task_id": "TASK-001" }                    // animus.queue.enqueue (task)
{ "subject_id": "blog:BLOG-001" }            // animus.queue.enqueue (BaaS dynamic kind — kernel resolves the kind)
{ "subject_id": "task:TASK-001" }            // animus.queue.hold / release / drop
{ "subject_ids": ["task:TASK-001", "task:TASK-002"] } // animus.queue.hold / release / drop (bulk)
{ "subject_ids": ["task:TASK-003", "task:TASK-001"] } // animus.queue.reorder
```

`animus.daemon.start` runs the same startup plugin preflight as the CLI. Use
`auto_install` when you want the daemon to remediate missing required plugins
from its recommended defaults before continuing; use `skip_preflight` only for
dev or intentionally degraded runs.

## Output and Logs Operations

Use output tools for run artifacts and structured execution streams. Use
`animus.logs.tail` for recent daemon-level logs. This MCP tool is a bounded
pull, not a live follow stream.

```json
{ "run_id": "run-abc123" }                  // animus.output.run
{ "run_id": "run-abc123", "entries": true } // animus.output.jsonl
{ "run_id": "run-abc123", "limit": 25 }     // animus.output.tail
{ "run_id": "run-abc123", "phase_id": "implementation" } // output.monitor
{ "limit": 100, "level": "warn" }           // animus.logs.tail
```

## Agent, Memory, Skill, and Plugin Operations

Direct agent controls:

```json
{ "tool": "codex", "model": "gpt-5.4", "prompt": "Investigate the flaky test" }
{ "run_id": "run-abc123", "action": "terminate" }
```

Project-scoped durable memory:

```json
{ "agent_id": "implementation", "text": "Use the new plugin router", "source": "postmortem" }
{ "agent_id": "implementation" }
{ "agent_id": "implementation", "prefix": "decision:" }
```

### Multi-phase coordination: implementation agent leaves findings for the review agent

Memory and messaging only pay off when agents are coached to use them and the
profiles declare the capability. A phase agent whose profile enables `memory`
or `communication` gets a short coaching paragraph appended to its prompt (the
"Coordination tools" section), plus recent memory/messages loaded into context
automatically. Phases without the capability get neither — no prompt bloat.

Worked example: the implementation phase records a decision the review phase
must honor, and pings the reviewer on a shared channel.

```yaml
# .animus/workflows.yaml
agents:
  implementation:
    capabilities: { memory: true }
    memory:
      enabled: true
      max_entries: 200        # FIFO cap; oldest entries trim on append
    communication:
      enabled: true
      channels: [engineering]
    mcp_servers: [animus]     # exposes animus.agent.message.send to the agent
  review:
    capabilities: { memory: true }
    memory: { enabled: true }
    communication:
      enabled: true
      channels: [engineering]
    mcp_servers: [animus]

agent_channels:
  engineering:
    participants: [implementation, review]
```

During the implementation phase the agent calls (memory via the injected
memory MCP server, which exposes `animus.memory.*`; messaging via the full
`animus` server):

```json
{ "agent_id": "implementation", "text": "decision: kept the legacy column for back-compat; do not flag the duplicate index as dead", "source": "phase:implementation" }  // animus.memory.append
{ "channel": "engineering", "from": "implementation", "to": "review", "text": "Heads up: intentional duplicate index, see memory decision." }  // animus.agent.message.send
```

When the review phase runs next, its prompt already carries the channel
message (the reviewer is a participant and the named recipient), so it knows
to look for the rationale. Memory is per-agent: the automatic prompt
injection only surfaces an agent's *own* recent entries, so the reviewer does
not see `implementation` memory automatically — it fetches it explicitly with
`animus.memory.get` `{ "agent_id": "implementation" }`. Meanwhile any later
phase run by the `implementation` profile sees the decision in its own prompt
without a lookup. Equivalent CLI form for scripts or manual handoff:

```bash
animus agent memory append --agent implementation \
  --text "decision: kept the legacy column for back-compat" --source phase:implementation
animus agent message send --channel engineering --from implementation --to review \
  --text "Heads up: intentional duplicate index, see memory decision."
```

Each successful append/clear/send emits a single `agent-memory-updated` or
`agent-message-sent` record to `animus daemon events`, so operators can watch
coordination without reading the store. Reads (`get`/`list`) emit nothing.

Skills:

```json
{ "query": "review" }        // animus.skill.search
{ "name": "code-review" }    // animus.skill.get
```

`animus.skill.get` (and `animus.skill.create` / `animus.skill.update`) include
a non-fatal `warnings` array when the definition declares an
`activation.tools` or `adapters` entry that is not a built-in tool id
(`claude`, `codex`, `gemini`, `opencode`, `oai-runner`) — unless a custom CLI
tool with that exact id is configured, such an entry never matches at
runtime, so the skill would silently never activate for it.

Plugins:

```json
{}                                             // animus.plugin.list
{ "name": "animus-provider-claude" }           // animus.plugin.info / ping / uninstall
{ "name": "animus-provider-claude", "method": "models/list" } // plugin.call
{ "query": "subject backend" }                 // animus.plugin.search
{ "kind": "subject_backend" }                  // animus.plugin.browse
{ "name": "animus-provider-claude" }           // animus.plugin.update
```

## Permission Modes

Spawned provider CLIs run with their own default permission posture unless a
permission mode is configured. Two surfaces feed it:

- Agent profile: set `permission_mode` on an `agents:` entry in workflow YAML
  (or a phase `runtime:` block, which wins over the profile). See
  [Workflow YAML](../reference/workflow-yaml.md#agents).
- CLI flag: `--permission-mode MODE` on `animus agent run` and
  `animus chat send` overrides any configured value.

The value is forwarded verbatim to the provider; it is provider-specific:

| Provider | Accepted modes | Mapped flag |
|---|---|---|
| claude | `default`, `acceptEdits`, `bypassPermissions`, `plan` | `--permission-mode MODE` |
| codex | `untrusted`, `on-failure`, `on-request`, `never` | `-c approval_policy="<mode>"` |
| gemini | `default`, `auto_edit`, `yolo` | approval-mode mapping |

A value outside the union of known modes prints a stderr warning but still
passes through unchanged, so future provider modes work without an Animus
release.

### `permission_mode` and `approval_policy` compose

A profile may set BOTH `permission_mode` and `approval_policy`; they are two
orthogonal layers and BOTH take effect — neither overrides the other:

- **`permission_mode` is the transport-level guard.** It rides the typed
  `SessionRequest.permission_mode` and maps to the provider CLI's own
  permission flag (the table above), deciding whether the provider acts
  autonomously or escalates a sensitive action at all.
- **`approval_policy` is the kernel inbox layer.** Its mere presence on the
  resolved profile flips `extras.approvals = true`, which makes the provider
  route escalations through `animus.agent.request_approval`. When such a
  request reaches the kernel, `ApprovalPolicy::evaluate` auto-allows /
  auto-denies / asks (auto_deny wins, matched against `tool_name` when
  present, else `action`).

In effect `permission_mode` governs whether the provider asks, and
`approval_policy` governs what happens to the asks that reach the kernel —
they compose rather than conflict. (Independently, an explicit
`--permission-mode` flag still wins over the profile's `permission_mode`, and
the `--approvals` flag forces kernel-mediated approvals even when no
`approval_policy` is declared.)

## Human-in-the-Loop Questions and Approvals

Agents that hit an ambiguity or a sensitive action mid-run can park on a human
answer without any protocol changes — the round-trip is an MCP tool call
against the same injected `animus` server:

```json
{ "agent_id": "swe", "question": "Migrate in place or copy table?", "options": ["in place", "copy"] }  // animus.agent.ask (flat)
{ "agent_id": "swe", "questions": [{ "question": "Which sections?", "header": "Sections", "options": [{ "label": "Intro" }, { "label": "Conclusion" }], "multi_select": true }] }  // animus.agent.ask (structured)
{ "agent_id": "swe", "action": "git push --force to main", "tool_name": "git.push" }                   // animus.agent.request_approval
```

The structured `questions[]` form (multi-question, multi-select, described
options) gives codex/gemini/opencode the same expressiveness claude has via its
native AskUserQuestion channel; the answer returns as
`{ answers: { "<question>": "<label | [labels] | text>" }, response?, answer }`,
where `answer` is a readable join kept for back-compat.

Both tools write a pending interaction under
`~/.animus/REPO_SCOPE/interactions/` and wait in one of two modes, selected
by the optional `wait` parameter:

- **`wait: "block"`** (default for ad-hoc `animus agent run` / `animus chat`)
  — the call polls until a human answers via
  `animus agent interactions answer ID` (or the `animus.interactions.answer`
  tool), or the timeout elapses (default 600s, max 3600s). `animus.agent.ask`
  times out with a structured error telling the agent to proceed with its best
  judgment; `animus.agent.request_approval` times out as a deny (fail closed).
- **`wait: "suspend"`** (default when the serving MCP process is pinned to a
  workflow via `animus mcp serve --workflow-id ID` or the
  `ANIMUS_MCP_WORKFLOW_ID` env var) — the tool records the pending
  interaction (bound to the pinned workflow id), pauses the workflow through
  the service API, stamps the interaction id into the phase session
  checkpoint, and returns immediately with
  `{ status: "pending", interaction_id, instruction }`. The instruction tells
  the agent to summarize its in-progress state and end the turn cleanly — no
  pool slot or process stays parked.

The payload may downgrade suspend→block; a block→suspend request on an
unpinned server is ignored with a warning (there is no workflow to resume).

Answering a suspended interaction resumes the workflow: the answer paths
(CLI `animus agent interactions answer` and `animus.interactions.answer`)
detect a suspend-created record bound to a paused workflow (block-mode
records never trigger a resume, even when their payload carried a
`workflow_id`) and trigger the same detached-runner
resume as `animus workflow resume`, with the decision threaded in as
feedback ("Approval granted/denied for ACTION: MESSAGE. Continue." /
"Answer to your question \"QUESTION\": ANSWER. Continue."). A resume
spawn failure never fails the answer — the response carries a
`workflow_resume.guidance` field with the exact `animus workflow resume ID`
command to run by hand. Paused workflows are exempt from the daemon's
orphaned-workflow recovery, so a suspended run waits indefinitely for its
answer (`animus status` surfaces it).

Before escalating, `animus.agent.request_approval` consults the agent
profile's `approval_policy` (`auto_allow` / `auto_deny` / `default`, declared
in the workflow YAML `agents:` block). Patterns match the request's
`tool_name` when present, otherwise its `action`, with the same `*`-glob
semantics as tool policies; `auto_deny` wins on overlap and `default` is one
of `ask` (escalate), `allow`, or `deny`.

Both blocking tools are bound to the server's own project scope (no
`project_root` override). The agent identity can be pinned with
`animus mcp serve --agent-id ID` (the ad-hoc `--agent` injection path
appends it automatically) or the `ANIMUS_MCP_AGENT_ID` env var, so the
payload `agent_id` cannot select a sibling profile with a looser policy.

The non-blocking management surface for inbox UIs (registered only when the
server runs as `animus mcp serve --management`; the default agent-injected
server omits these tools so an agent cannot answer its own approvals):

```json
{ "all": false }                                  // animus.interactions.list
{ "id": "UUID", "text": "use the copy table" }  // animus.interactions.answer (question)
{ "id": "UUID", "decision": "deny", "message": "too risky" }  // animus.interactions.answer (approval)
```

Interaction lifecycle events (`interaction_created`, `interaction_answered`,
`interaction_expired`) are appended to the daemon event log, so
`animus daemon events` surfaces pending escalations without polling the store.
Each event carries a one-line `summary` and a ready-to-run `answer_command`.
When the daemon is running with notifier plugins installed, a per-tick
watcher fans fresh interaction events out to them (Slack / Telegram / HTTP)
best-effort — notifier failures never block or fail the interaction.

## Error Remediation

Tool failures return a structured payload with the wrapped CLI `error`,
`exit_code`, and `stderr`. For the determinate failure classes the payload
also carries a machine-actionable `remediation` object — `missing_plugin`
(with the exact `install_command` to run), `daemon_not_running` (with
`next_step: "animus daemon start"`), or `invalid_input` (with a `help` hint).
Act on `remediation` first when present; see the
[remediation schema](../reference/mcp-tools.md#error-remediation) for details.

## Recommended Flow

1. Discover or create a subject with `animus.subject.list` or `animus.subject.create`. `animus.subject.list` returns a bounded page by default (`limit` defaults to 50) plus a `next_cursor` (and `total` when the backend reports it); pass `cursor` to fetch the next page, or `limit: 0` to remove the cap. Prefer paging over `limit: 0` for large boards to keep results small.
2. Mark it ready with `animus.subject.status`.
3. Start work with `animus.workflow.run` or let the daemon schedule it.
4. Observe progress with `animus.workflow.list`, `animus.output.*`, and `animus.logs.tail`.
5. Use `animus.memory.*` for durable agent notes and `animus.plugin.*` when a plugin capability is missing or needs inspection.

## Notes

- `animus.task.*` and `animus.requirements.*` are no longer part of the live
  MCP surface.
- `animus.plugin.*` now includes both installed-plugin tools and marketplace
  discovery tools.
- Two memory families exist and are NOT interchangeable. `animus.memory.*`
  (4 tools) is an **any-agent-id document store** — you pass `agent_id`
  explicitly and can read or write any agent's memory. It serves external
  clients AND is the family the injected memory sidecar actually exposes to a
  memory-capable workflow agent (`capabilities.memory: true` injects
  `animus mcp memory`, whose tools are `animus.memory.*` only). The
  `animus.agent.memory.*` wrappers (3 tools) validate the agent profile and
  live **only on the full `animus` MCP server** — a phase agent sees them only
  when its profile lists `animus` in `mcp_servers`. From inside a phase,
  record findings with `animus.memory.append` and your own `agent_id`; from an
  operator/dashboard client, use `animus.memory.*` to inspect or seed another
  agent's memory. See
  [MCP Tools — memory families](../reference/mcp-tools.md) for the full split.
- Ad-hoc agents (`animus chat send`, `animus agent run`) now receive the MCP
  servers their selected profile/skill declares, resolved by name against the
  project's `mcp_servers` map — a trading agent gets the trading servers, a
  marketing agent gets the marketing ones. Use `--agent` / `--skill` to select
  the scope, `--mcp-server <name>` to add more, and `--no-animus-mcp` to drop
  the built-in `animus` server. A plain chat (no profile/skill) defaults to the
  `animus` server only. The same resolved set is mirrored onto both the
  runtime contract and the provider-facing `mcp_servers` request field, and
  any server with an `oauth:` block is routed through `animus-mcp-proxy`
  instead of exposing a bearer token directly. See the per-agent MCP server
  section in the [CLI Command Surface](../reference/cli/index.md).
- Workflow phase agents receive skills the same way: the union of the phase's
  `skills:` list and the executing agent profile's `skills:` is resolved
  daemon-side at dispatch (same scoped sources and trust rules as `--skill`)
  and shipped to the workflow runner, which applies activation gating against
  the selected tool/model and injects prompt fragments, tool policy, and
  skill-declared MCP servers into the phase contract. Missing skill names
  warn and are recorded on phase metadata (`requested_skills` /
  `resolved_skills` / `applied_skills`) instead of failing the run. See the
  phases section of the
  [Workflow YAML reference](../reference/workflow-yaml.md#phases).
- Skills now apply FULLY on the ad-hoc paths (`animus agent run` and
  `animus chat send`), not just their MCP servers + tool policy: the
  `--skill`'s prompt prefix/suffix/directives wrap the prompt, its
  `prompt.system` fragments ride the session's `system_prompt` (an explicit
  `--context-json system_prompt` comes first), and its `extra_args`, `env`,
  and `codex_config_overrides` are forwarded the same way workflow phases
  forward them (its `model` preference and `timeout_secs` apply when no
  explicit value is given). Precedence: explicit CLI flags / context-json > skill >
  defaults; a caller-supplied `--runtime-contract-json` disables skill
  application entirely. On `animus chat send` the skill binds once per send
  invocation; launch-affecting skills force full-history replay instead of
  native resume so the flags apply to every turn's provider process. See the
  ad-hoc skill application section in the
  [CLI Command Surface](../reference/cli/index.md).

See also: [MCP Tools Reference](../reference/mcp-tools.md),
[CLI Command Surface](../reference/cli/index.md), and
[Writing Workflows](writing-workflows.md).
