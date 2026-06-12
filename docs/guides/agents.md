# Working with Animus via MCP Tools

This guide explains the current MCP surface exposed by `animus mcp serve`.
Each built-in tool maps to an `animus` CLI command and accepts JSON input.

For the full parameter table, see [MCP Tools Reference](../reference/mcp-tools.md).

## Overview

Animus currently exposes **81 built-in MCP tools** across these families:

| Group | Tools | Purpose |
|---|---:|---|
| `animus.agent.*` | 12 | Agent profiles, runs, memory, agent messaging, and blocking human-in-the-loop questions/approvals |
| `animus.daemon.*` | 11 | Daemon lifecycle, health, events, and config |
| `animus.subject.*` | 6 | Task, requirement, and external subject backends |
| `animus.workflow.*` | 16 | Workflow execution, control, and definition inspection |
| `animus.queue.*` | 7 | Dispatch queue inspection and mutation |
| `animus.output.*` | 6 | Run output, artifacts, JSONL, and live monitoring |
| `animus.skill.*` | 5 | Skill discovery, inspection, and project-scoped authoring |
| `animus.memory.*` | 4 | Project-scoped durable agent memory |
| `animus.plugin.*` | 9 | Installed-plugin inspection/mutation plus marketplace discovery/update |
| `animus.logs.*` | 1 | Tail log entries from the active log backend |
| `animus.interactions.*` | 2 | Non-blocking inbox over pending agent questions and approval requests (`animus mcp serve --management` only) |
| `animus.tools.*` | 2 | Tool discovery over the live registry: ranked keyword search plus a grouped one-line catalog |

**Tool discovery.** When you are unsure which tool fits an intent — or your
context budget is too tight to carry all 81 schemas — start with
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

## Workflow Operations

Use workflows to execute work for a task, a requirement, or a freeform title.

```json
// Async run via daemon
{ "task_id": "TASK-001" }

// Sync execution in-process (animus.workflow.execute)
{ "task_id": "TASK-001", "phase": "implementation", "model": "gpt-5.4" }

// Requirement-linked execution
{ "requirement_id": "REQ-001", "workflow_ref": "standard-workflow" }

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
{ "workflow_id": "wf-abc123", "phase_id": "po-review" } // phase.approve
```

The CLI also exposes `animus workflow phase reject`, but the built-in MCP
surface currently exposes only `animus.workflow.phase.approve`.

## Daemon and Queue Operations

Use the daemon tools for autonomous scheduling and queue tools for explicit
dispatch control.

```json
{}                                          // animus.daemon.status / health / agents
{ "pool_size": 3, "auto_install": true }                    // animus.daemon.start (always detaches)
{ "skip_preflight": true }                  // animus.daemon.start (dev escape hatch)
{ "auto_run_ready": true, "pool_size": 4 }  // animus.daemon.config-set
{ "limit": 50 }                              // animus.daemon.events
{ "project_root": "/repo" }                  // animus.queue.list / stats
{ "subject_id": "task:TASK-001" }            // animus.queue.hold / release / drop
{ "subject_ids": ["task:TASK-001", "task:TASK-002"] } // animus.queue.hold / release / drop (bulk)
{ "subject_ids": ["task:TASK-003", "task:TASK-001"] } // animus.queue.reorder
```

`animus.daemon.start` runs the same startup plugin preflight as the CLI. Use
`auto_install` when you want the daemon to remediate missing required plugins
from its recommended defaults before continuing; use `skip_preflight` only for
dev or intentionally degraded runs.

## Output, Logs, and Runner Operations

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

Skills:

```json
{ "query": "review" }        // animus.skill.search
{ "name": "code-review" }    // animus.skill.get
```

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

## Human-in-the-Loop Questions and Approvals

Agents that hit an ambiguity or a sensitive action mid-run can park on a human
answer without any protocol changes — the round-trip is an MCP tool call
against the same injected `animus` server:

```json
{ "agent_id": "swe", "question": "Migrate in place or copy table?", "options": ["in place", "copy"] }  // animus.agent.ask
{ "agent_id": "swe", "action": "git push --force to main", "tool_name": "git.push" }                   // animus.agent.request_approval
```

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

## Recommended Flow

1. Discover or create a subject with `animus.subject.list` or `animus.subject.create`.
2. Mark it ready with `animus.subject.status`.
3. Start work with `animus.workflow.run` or let the daemon schedule it.
4. Observe progress with `animus.workflow.list`, `animus.output.*`, and `animus.logs.tail`.
5. Use `animus.memory.*` for durable agent notes and `animus.plugin.*` when a plugin capability is missing or needs inspection.

## Notes

- `animus.task.*` and `animus.requirements.*` are no longer part of the live
  MCP surface.
- `animus.plugin.*` now includes both installed-plugin tools and marketplace
  discovery tools.
- `animus.memory.*` is always exposed from the top-level MCP server; injected
  workflow agents only see it when their profile enables memory capability.
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
