# Working with AO via MCP Tools — Agent Guide

This guide explains how AI agents (and MCP clients) interact with the AO orchestrator through its MCP tool surface. Every tool maps 1:1 to an `ao` CLI command and accepts JSON input.

For the full tool table with parameters, see [MCP Tools Reference](../reference/mcp-tools.md).

---

## Overview

AO exposes ~69 MCP tools organized into 8 groups:

| Group | Tools | Purpose |
|-------|-------|---------|
| `ao.task.*` | 20 | Task lifecycle management |
| `ao.workflow.*` | 14 | Workflow execution and control |
| `ao.daemon.*` | 11 | Background scheduler management |
| `ao.requirements.*` | 6 | Requirements tracking |
| `ao.queue.*` | 6 | Dispatch queue management |
| `ao.output.*` | 6 | Agent output and monitoring |
| `ao.agent.*` | 3 | Direct agent execution |
| `ao.runner.*` | 3 | Runner process health |

Every tool accepts an optional `project_root` parameter to specify which project to operate on. If omitted, the current working directory is used.

---

## Task Management (`ao.task.*`)

Tasks are the primary unit of work. Each task has an ID (e.g., `TASK-001`), title, description, status, priority, and optional metadata like checklists, dependencies, and deadlines.

### Creating Tasks

```json
// ao.task.create
{
  "title": "Add retry logic to HTTP client",
  "description": "Implement exponential backoff for rate-limited responses",
  "priority": "high",
  "task_type": "feature",
  "tags": ["http", "resilience"]
}
```

### Querying Tasks

```json
// ao.task.list — filter by status, priority, type, tags, search text
{ "status": "in-progress", "priority": "high", "limit": 10 }

// ao.task.get — full details for a single task
{ "id": "TASK-001" }

// ao.task.prioritized — tasks sorted by priority, respecting dependencies
{ "limit": 10 }

// ao.task.next — the single highest-priority ready task
{}

// ao.task.stats — aggregate counts by status, priority, type
{}

// ao.task.history — workflow dispatch history for a task
{ "id": "TASK-001" }
```

### Updating Tasks

```json
// ao.task.status — change task status
// Valid: backlog, todo, ready, in_progress, blocked, on_hold, done, cancelled
{ "id": "TASK-001", "status": "in-progress" }

// ao.task.update — update any fields
{ "id": "TASK-001", "title": "New title", "priority": "critical" }

// ao.task.assign — assign to user or agent
{ "id": "TASK-001", "assignee": "agent:claude", "assignee_type": "agent", "model": "claude-sonnet-4-6" }

// ao.task.set-priority
{ "task_id": "TASK-001", "priority": "critical" }

// ao.task.set-deadline — set or clear (omit deadline to clear)
{ "task_id": "TASK-001", "deadline": "2026-03-15" }
```

### Checklists

```json
// ao.task.checklist-add
{ "id": "TASK-001", "description": "Write unit tests for backoff" }

// ao.task.checklist-update — use ao.task.get first to find item_id
{ "id": "TASK-001", "item_id": "chk-1", "completed": true }
```

### Pause, Resume, Cancel

```json
// ao.task.pause — prevents daemon from scheduling
{ "task_id": "TASK-001" }

// ao.task.resume — re-enables scheduling
{ "task_id": "TASK-001" }

// ao.task.cancel — permanently cancel
{ "task_id": "TASK-001", "confirm": "yes" }
```

### Bulk Operations

```json
// ao.task.bulk-status — batch status updates
{
  "updates": [
    { "id": "TASK-001", "status": "done" },
    { "id": "TASK-002", "status": "ready" }
  ],
  "on_error": "continue"
}

// ao.task.bulk-update — batch field updates
{
  "updates": [
    { "id": "TASK-001", "priority": "high" },
    { "id": "TASK-002", "assignee": "agent:claude" }
  ],
  "on_error": "stop"
}
```

---

## Workflow Engine (`ao.workflow.*`)

Workflows orchestrate multi-phase execution of tasks. A workflow runs phases (e.g., research → implementation → review) sequentially, with each phase executing an AI agent.

### Running Workflows

```json
// ao.workflow.run — async via daemon (returns immediately)
{ "task_id": "TASK-001" }

// ao.workflow.run — with specific workflow definition
{ "task_id": "TASK-001", "workflow_ref": "default" }

// ao.workflow.execute — synchronous (blocks until complete, no daemon needed)
{ "task_id": "TASK-001" }

// ao.workflow.execute — run a specific phase only
{ "task_id": "TASK-001", "phase": "implementation", "model": "claude-sonnet-4-6" }

// ao.workflow.run-multiple — batch workflow dispatch
{
  "runs": [
    { "task_id": "TASK-001" },
    { "task_id": "TASK-002", "workflow_ref": "quick" }
  ],
  "on_error": "continue"
}
```

### Monitoring Workflows

```json
// ao.workflow.get — full workflow state
{ "id": "wf-abc123" }

// ao.workflow.list — list all workflow executions
{ "limit": 10 }

// ao.workflow.decisions — decision log during execution
{ "id": "wf-abc123" }

// ao.workflow.checkpoints.list — saved state checkpoints
{ "id": "wf-abc123" }
```

### Controlling Workflows

```json
// ao.workflow.pause
{ "id": "wf-abc123" }

// ao.workflow.resume
{ "id": "wf-abc123" }

// ao.workflow.cancel — permanently stop
{ "id": "wf-abc123", "confirm": "yes" }
```

### Phase & Definition Inspection

```json
// ao.workflow.phases.list — all available phase definitions
{}

// ao.workflow.phases.get — details of a specific phase
{ "phase": "implementation" }

// ao.workflow.definitions.list — all workflow definitions
{}

// ao.workflow.config.get — effective workflow configuration
{}

// ao.workflow.config.validate — check config for errors
{}

// ao.workflow.phase.approve — approve a gated phase
{ "workflow_id": "wf-abc123", "phase_id": "review", "feedback": "Looks good" }
```

---

## Daemon Management (`ao.daemon.*`)

The daemon is the background scheduler that picks up ready tasks, dispatches workflows, manages agents, and handles auto-merge/auto-PR.

### Lifecycle

```json
// ao.daemon.start — start the daemon
{}

// ao.daemon.start — with options
{
  "autonomous": true,
  "interval_secs": 5,
  "max_agents": 3,
  "auto_run_ready": true,
  "phase_timeout_secs": 1800
}

// ao.daemon.stop — graceful shutdown
{}

// ao.daemon.pause — stop picking up new work (in-progress continues)
{}

// ao.daemon.resume — resume scheduling
{}
```

### Monitoring

```json
// ao.daemon.status — is it running?
{}

// ao.daemon.health — detailed metrics (uptime, agents, capacity)
{}

// ao.daemon.agents — list currently running agents
{}

// ao.daemon.events — recent event history
{}

// ao.daemon.logs — read log file
{ "limit": 100 }

// ao.daemon.logs — search for errors
{ "search": "error" }
```

### Configuration

```json
// ao.daemon.config — read current settings
{}

// ao.daemon.config-set — update settings
{
  "auto_merge": true,
  "auto_pr": true,
  "auto_commit_before_merge": true,
  "auto_prune_worktrees_after_merge": true,
  "auto_run_ready": true
}
```

---

## Agent Execution (`ao.agent.*`)

Agents are AI CLI tool processes (claude, codex, gemini) managed by the runner. You can run agents directly without the daemon.

```json
// ao.agent.run — launch an agent
{
  "tool": "claude",
  "model": "claude-sonnet-4-6",
  "prompt": "Fix the failing test in src/lib.rs",
  "detach": true
}

// ao.agent.status — check if agent is running/completed/failed
{ "run_id": "abc123" }

// ao.agent.control — pause, resume, or terminate
{ "run_id": "abc123", "action": "terminate" }
```

---

## Output & Monitoring (`ao.output.*`)

View what agents have produced during execution.

```json
// ao.output.run — full stdout/stderr from a run
{ "run_id": "abc123" }

// ao.output.tail — recent events (quick check on progress)
{ "run_id": "abc123", "limit": 50 }

// ao.output.tail — filter by event type
{ "task_id": "TASK-001", "event_types": ["stdout", "stderr"] }

// ao.output.monitor — stream live output
{ "run_id": "abc123" }

// ao.output.jsonl — structured event log
{ "run_id": "abc123", "entries": true }

// ao.output.artifacts — files generated during execution
{ "execution_id": "exec-abc123" }

// ao.output.phase-outputs — persisted workflow phase outputs
{ "workflow_id": "wf-abc123" }

// ao.output.phase-outputs — with specific phase
{ "workflow_id": "wf-abc123", "phase_id": "implementation" }
```

---

## Requirements (`ao.requirements.*`)

Requirements are high-level objectives that tasks are derived from.

```json
// ao.requirements.list
{ "limit": 20 }

// ao.requirements.get
{ "id": "REQ-001" }

// ao.requirements.create
{
  "title": "HTTP client resilience",
  "description": "All HTTP calls should handle transient failures gracefully",
  "priority": "high"
}

// ao.requirements.update
{ "id": "REQ-001", "status": "accepted" }

// ao.requirements.delete
{ "id": "REQ-001" }

// ao.requirements.refine — improve requirements, optionally with AI
{ "id": ["REQ-001"], "focus": "tighten acceptance criteria", "use_ai": true }
```

---

## Queue Management (`ao.queue.*`)

The dispatch queue controls the order in which the daemon picks up work.

```json
// ao.queue.list — view queued dispatches
{}

// ao.queue.stats — aggregate depth and status counts
{}

// ao.queue.enqueue — manually add to queue
{ "task_id": "TASK-001" }

// ao.queue.hold — prevent dispatch without removing
{ "subject_id": "TASK-001" }

// ao.queue.release — resume dispatch eligibility
{ "subject_id": "TASK-001" }

// ao.queue.reorder — set preferred dispatch order
{ "subject_ids": ["TASK-003", "TASK-001", "TASK-002"] }
```

---

## Runner Health (`ao.runner.*`)

The runner is a separate process that spawns CLI tools. It's managed by the daemon but can be checked independently.

```json
// ao.runner.health — is the runner up and has capacity?
{}

// ao.runner.orphans-detect — find leaked processes
{}

// ao.runner.restart-stats — uptime and restart history
{}
```

---

## Common Workflows

### 1. Start fresh: create a task and run it

```
ao.task.create        → { "title": "...", "priority": "high" }
ao.task.status        → { "id": "TASK-XXX", "status": "ready" }
ao.workflow.execute   → { "task_id": "TASK-XXX" }
```

### 2. Let the daemon handle everything

```
ao.daemon.start       → { "autonomous": true, "auto_run_ready": true }
ao.task.create        → { "title": "...", "priority": "high" }
ao.task.status        → { "id": "TASK-XXX", "status": "ready" }
                        (daemon picks it up automatically)
ao.daemon.agents      → {} (check what's running)
ao.output.tail        → { "task_id": "TASK-XXX" }
```

### 3. Monitor and debug

```
ao.daemon.status      → {} (is it running?)
ao.daemon.health      → {} (capacity and metrics)
ao.daemon.logs        → { "search": "error" }
ao.runner.health      → {} (runner process ok?)
ao.runner.orphans-detect → {} (leaked processes?)
```

### 4. Batch process multiple tasks

```
ao.task.bulk-status   → { "updates": [{"id":"TASK-001","status":"ready"}, ...] }
ao.workflow.run-multiple → { "runs": [{"task_id":"TASK-001"}, {"task_id":"TASK-002"}] }
```

### 5. Requirements-driven planning

```
ao.requirements.create → { "title": "...", "description": "..." }
ao.requirements.refine → { "id": ["REQ-001"], "use_ai": true }
ao.task.create         → { "title": "...", "linked_requirement": ["REQ-001"] }
```

### 6. Queue management

```
ao.queue.list          → {} (see what's queued)
ao.queue.hold          → { "subject_id": "TASK-003" } (hold back a task)
ao.queue.reorder       → { "subject_ids": ["TASK-001", "TASK-002"] } (prioritize)
ao.queue.release       → { "subject_id": "TASK-003" } (let it dispatch)
```

---

## Pagination

All list tools support:

| Parameter | Type | Default | Max | Description |
|-----------|------|---------|-----|-------------|
| `limit` | integer | 25 | 200 | Items per page |
| `offset` | integer | 0 | — | Items to skip |
| `max_tokens` | integer | 3000 | 12000 | Token budget for response compaction |

Responses use the `ao.mcp.list.result.v1` envelope with pagination metadata.

## Batch Operations

Batch tools (`ao.task.bulk-status`, `ao.task.bulk-update`, `ao.workflow.run-multiple`) share:

| Parameter | Values | Description |
|-----------|--------|-------------|
| `on_error` | `"stop"` (default), `"continue"` | Whether to halt or proceed on failure |

Max 100 items per batch call. Responses use `ao.mcp.batch.result.v1` with per-item results.

---

## Tool Sequencing Tips

Many tools are designed to be used in sequence. The tool descriptions include `Sequencing:` hints:

- **Before creating**: `ao.task.list` or `ao.requirements.list` to check for duplicates
- **Before updating**: `ao.task.get` to verify current state
- **Before running workflows**: `ao.task.get` to verify the task exists
- **Before starting agents**: `ao.runner.health` to verify capacity
- **After starting daemon**: `ao.daemon.status` or `ao.daemon.health` to verify
- **After running workflows**: `ao.output.tail` or `ao.workflow.get` to monitor

See also: [MCP Tools Reference](../reference/mcp-tools.md), [Task Management](task-management.md), [Daemon Operations](daemon-operations.md), [Writing Workflows](writing-workflows.md).
