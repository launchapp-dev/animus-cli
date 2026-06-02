# Working with Animus via MCP Tools

This guide explains the current MCP tool surface exposed by `animus mcp serve`.
Each tool maps to an `animus` CLI command and accepts JSON input.

For the full parameter table, see [MCP Tools Reference](../reference/mcp-tools.md).

## Overview

Animus currently exposes **77 built-in MCP tools** across these families:

| Group | Tools | Purpose |
|---|---:|---|
| `animus.agent.*` | 10 | Agent profiles, runs, memory, and agent messaging |
| `animus.daemon.*` | 11 | Daemon lifecycle, health, events, and config |
| `animus.subject.*` | 6 | Task, requirement, and external subject backends |
| `animus.workflow.*` | 16 | Workflow execution, control, and definition inspection |
| `animus.queue.*` | 7 | Dispatch queue inspection and mutation |
| `animus.output.*` | 6 | Run output, artifacts, JSONL, and live monitoring |
| `animus.runner.*` | 4 | Runner health and orphan cleanup |
| `animus.skill.*` | 3 | Skill discovery and inspection |
| `animus.memory.*` | 4 | Project-scoped durable agent memory |
| `animus.plugin.*` | 9 | Installed-plugin inspection/mutation plus marketplace discovery/update |
| `animus.logs.*` | 1 | Tail log entries from the active log backend |

Most project-scoped tools accept an optional `project_root`. Marketplace tools
may omit it because they operate on the public registry. Plugin mutation tools
such as `animus.plugin.install` and `animus.plugin.uninstall` can still take
`project_root` so project-local `.animus/plugins.lock` updates stay scoped to
the target repo when present.

The total includes both the CLI-shaped `animus.agent.memory.*` wrappers and the
top-level `animus.memory.*` document-oriented surface composed into
`animus mcp serve`.

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
{ "autonomous": true, "pool_size": 3, "auto_install": true } // animus.daemon.start
{ "skip_preflight": true }                  // animus.daemon.start (dev escape hatch)
{ "auto_run_ready": true, "pool_size": 4 }  // animus.daemon.config-set
{ "limit": 50 }                              // animus.daemon.events
{ "project_root": "/repo" }                  // animus.queue.list / stats
{ "subject_id": "task:TASK-001" }            // animus.queue.hold / release / drop
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

Runner tools are operational checks:

```json
{}                             // animus.runner.health / restart-stats
{ "run_id": ["run-abc123"] }   // animus.runner.orphans-cleanup
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

See also: [MCP Tools Reference](../reference/mcp-tools.md),
[CLI Command Surface](../reference/cli/index.md), and
[Writing Workflows](writing-workflows.md).
