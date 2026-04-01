# MCP Tools Reference

All MCP tools exposed by `ao mcp serve`. These tools allow AI agents to interact with the AO orchestrator over the Model Context Protocol. Each tool wraps an `ao` CLI command, accepting JSON input and returning structured results.

Every tool accepts an optional `project_root` parameter to override the default project root.

---

## Agent Control (3 tools)

| Tool | Description | Key Parameters |
|---|---|---|
| `ao.agent.run` | Launch an AI agent to execute work | `tool`, `model`, `prompt`, `cwd`, `timeout_secs`, `context_json`, `runtime_contract_json`, `detach`, `run_id`, `runner_scope`, `project_root` |
| `ao.agent.control` | Control a running agent (pause/resume/terminate) | `run_id`, `action` (`pause`, `resume`, `terminate`), `runner_scope` |
| `ao.agent.status` | Get status of an agent run | `run_id`, `runner_scope` |

---

## Daemon Management (11 tools)

| Tool | Description | Key Parameters |
|---|---|---|
| `ao.daemon.start` | Start the AO daemon for task scheduling and agent management | `pool_size` (alias: `max_agents`), `interval_secs`, `auto_run_ready`, `auto_merge`, `auto_pr`, `auto_commit_before_merge`, `auto_prune_worktrees_after_merge`, `startup_cleanup`, `resume_interrupted`, `reconcile_stale`, `stale_threshold_hours`, `max_tasks_per_tick`, `phase_timeout_secs`, `idle_timeout_secs`, `skip_runner`, `autonomous`, `runner_scope`, `project_root` |
| `ao.daemon.stop` | Stop the daemon gracefully | `project_root` |
| `ao.daemon.status` | Check if daemon is running and view basic state | `project_root` |
| `ao.daemon.health` | Get detailed health metrics (active agents, queue, capacity) | `project_root` |
| `ao.daemon.pause` | Pause the scheduler without stopping the daemon | `project_root` |
| `ao.daemon.resume` | Resume the scheduler after a pause | `project_root` |
| `ao.daemon.events` | List recent daemon events for debugging and monitoring | `limit`, `project_root` |
| `ao.daemon.agents` | List currently running agent tasks and their status | `project_root` |
| `ao.daemon.logs` | Read daemon process log file | `limit`, `search`, `project_root` |
| `ao.daemon.config` | Read current daemon automation settings | `project_root` |
| `ao.daemon.config-set` | Update daemon automation settings and notification config | `auto_merge`, `auto_pr`, `auto_commit_before_merge`, `auto_prune_worktrees_after_merge`, `auto_run_ready`, `pool_size` (alias: `max_agents`), `interval_secs`, `max_tasks_per_tick`, `stale_threshold_hours`, `phase_timeout_secs`, `idle_timeout_secs`, `notification_config_json`, `notification_config_file`, `clear_notification_config`, `project_root` |

---

## Task Operations (20 tools)

### Query Tools (6)

| Tool | Description | Key Parameters |
|---|---|---|
| `ao.task.list` | List tasks with filters | `status`, `priority`, `task_type`, `assignee_type`, `tag[]`, `risk`, `linked_requirement`, `linked_architecture_entity`, `search`, `limit`, `offset`, `max_tokens` |
| `ao.task.get` | Fetch full task details by ID | `id` |
| `ao.task.prioritized` | List tasks sorted by priority | `status`, `priority`, `assignee_type`, `search`, `limit`, `offset`, `max_tokens` |
| `ao.task.next` | Get the single highest priority ready task | `project_root` |
| `ao.task.stats` | Aggregate task metrics by status, priority, type | `project_root` |
| `ao.task.history` | View workflow dispatch history for a task | `id` |

### Mutation Tools (14)

| Tool | Description | Key Parameters |
|---|---|---|
| `ao.task.create` | Create a new task | `title`, `description`, `priority`, `task_type`, `linked_requirement[]`, `linked_architecture_entity[]`, `project_root` |
| `ao.task.update` | Update task fields | `id`, `title`, `description`, `priority`, `status`, `assignee`, `linked_architecture_entity[]`, `replace_linked_architecture_entities`, `input_json` |
| `ao.task.delete` | Delete a task (destructive) | `id`, `confirm`, `dry_run` |
| `ao.task.status` | Update task status | `id`, `status` |
| `ao.task.assign` | Assign task to user or agent | `id`, `assignee`, `assignee_type`, `agent_role`, `model` |
| `ao.task.pause` | Pause a running task | `id` |
| `ao.task.resume` | Resume a paused task | `id` |
| `ao.task.cancel` | Cancel a task | `id`, `confirm`, `dry_run` |
| `ao.task.set-priority` | Set task priority | `id`, `priority` |
| `ao.task.set-deadline` | Set or clear task deadline | `id`, `deadline` |
| `ao.task.checklist-add` | Add a checklist item to a task | `id`, `description` |
| `ao.task.checklist-update` | Toggle checklist item completion | `id`, `item_id`, `completed` |
| `ao.task.bulk-status` | Batch-update status for multiple tasks | `updates[]` (each: `id`, `status`), `on_error` |
| `ao.task.bulk-update` | Batch-update fields for multiple tasks | `updates[]` (each: `id` + fields), `on_error` |

---

## Workflow Operations (17 tools)

### Runtime Tools (10)

| Tool | Description | Key Parameters |
|---|---|---|
| `ao.workflow.run` | Start a workflow for a task (async, via daemon) | `task_id`, `requirement_id`, `title`, `description`, `workflow_ref`, `input_json` |
| `ao.workflow.run-multiple` | Batch-run workflows for multiple tasks | `runs[]` (each: `task_id`, `workflow_ref`, `input_json`), `on_error` |
| `ao.workflow.execute` | Execute a workflow synchronously (no daemon) | `task_id`, `workflow_ref`, `phase`, `model`, `tool`, `phase_timeout_secs`, `input_json` |
| `ao.workflow.get` | Get full workflow state by ID | `id` |
| `ao.workflow.list` | List workflow executions | `status`, `workflow_ref`, `task_id`, `phase_id`, `search`, `sort`, `limit`, `offset`, `max_tokens` |
| `ao.workflow.context` | Get workflow pipeline context with phase outputs and verdicts | `id` |
| `ao.workflow.pause` | Pause a running workflow | `id`, `confirm`, `dry_run` |
| `ao.workflow.cancel` | Cancel a running workflow permanently | `id`, `confirm`, `dry_run` |
| `ao.workflow.resume` | Resume a paused workflow | `id` |
| `ao.workflow.phase.approve` | Approve a gated workflow phase | `workflow_id`, `phase_id`, `feedback` |

### Decision & Checkpoint Tools (2)

| Tool | Description | Key Parameters |
|---|---|---|
| `ao.workflow.decisions` | List decisions made during workflow execution | `id`, `limit`, `offset`, `max_tokens` |
| `ao.workflow.checkpoints.list` | List saved workflow state checkpoints | `id`, `limit`, `offset`, `max_tokens` |

### Definition Tools (5)

| Tool | Description | Key Parameters |
|---|---|---|
| `ao.workflow.phases.list` | List available phase definitions | `project_root` |
| `ao.workflow.phases.get` | Get a specific phase definition | `phase` |
| `ao.workflow.definitions.list` | List workflow definitions | `project_root` |
| `ao.workflow.config.get` | Read effective workflow configuration | `project_root` |
| `ao.workflow.config.validate` | Validate workflow config for errors | `project_root` |

---

## Requirements (6 tools)

| Tool | Description | Key Parameters |
|---|---|---|
| `ao.requirements.list` | List requirements with pagination | `limit`, `offset`, `max_tokens`, `status` |
| `ao.requirements.get` | Get full requirement details by ID | `id` |
| `ao.requirements.create` | Create a new requirement | `title`, `description`, `priority`, `acceptance_criterion[]` |
| `ao.requirements.update` | Update requirement fields | `id`, `title`, `description`, `priority`, `status`, `acceptance_criterion[]` |
| `ao.requirements.delete` | Delete a requirement | `id` |
| `ao.requirements.refine` | Refine requirements with optional AI assistance | `id[]`, `focus`, `use_ai`, `tool`, `model`, `timeout_secs`, `start_runner`, `input_json` |

---

## Queue Operations (7 tools)

| Tool | Description | Key Parameters |
|---|---|---|
| `ao.queue.list` | List queued subject dispatches | `project_root` |
| `ao.queue.stats` | Get aggregate queue depth and status counts | `project_root` |
| `ao.queue.enqueue` | Add a subject dispatch to the queue | `task_id`, `requirement_id`, `title`, `description`, `workflow_ref`, `input_json` |
| `ao.queue.reorder` | Set preferred dispatch order | `subject_ids[]` |
| `ao.queue.hold` | Hold a pending subject from dispatch | `subject_id` |
| `ao.queue.release` | Release a held subject for dispatch | `subject_id` |
| `ao.queue.drop` | Remove a queued subject dispatch permanently | `subject_id`, `project_root` |

---

## Output & Monitoring (6 tools)

| Tool | Description | Key Parameters |
|---|---|---|
| `ao.output.run` | Get stdout/stderr from an agent execution | `run_id` |
| `ao.output.tail` | Get most recent output/error/thinking events | `run_id`, `task_id`, `event_types[]`, `limit` |
| `ao.output.monitor` | Stream real-time output from a run, optionally scoped by task or phase | `run_id`, `task_id`, `phase_id` |
| `ao.output.jsonl` | Get structured JSONL event log | `run_id`, `entries` |
| `ao.output.artifacts` | Get files generated during execution | `execution_id` |
| `ao.output.phase-outputs` | Get persisted workflow phase outputs | `workflow_id`, `phase_id`, `project_root` |

---

## Runner (4 tools)

| Tool | Description | Key Parameters |
|---|---|---|
| `ao.runner.health` | Check runner process health and capacity | `project_root` |
| `ao.runner.orphans-detect` | Find orphaned runner processes | `project_root` |
| `ao.runner.orphans-cleanup` | Clean up orphaned runner processes by run ID | `run_id`, `project_root` |
| `ao.runner.restart-stats` | View runner uptime and restart history | `project_root` |

---

## List Tool Pagination

All list tools support pagination via these common parameters:

| Parameter | Type | Default | Max | Description |
|---|---|---|---|---|
| `limit` | integer | 25 | 200 | Maximum items to return |
| `offset` | integer | 0 | -- | Items to skip |
| `max_tokens` | integer | 3000 | 12000 | Token budget for response compaction (min: 256) |

List responses are wrapped in a guard envelope (`ao.mcp.list.result.v1`) that includes pagination metadata.

## Batch Tool Behavior

Batch tools (`ao.task.bulk-status`, `ao.task.bulk-update`, `ao.workflow.run-multiple`) accept an `on_error` parameter:

| Value | Behavior |
|---|---|
| `"continue"` | Process all items regardless of failures |
| `"stop"` | Stop processing after the first failure; remaining items are marked `"skipped"` |

Batch responses use the `ao.mcp.batch.result.v1` schema with a summary of succeeded/failed/skipped counts and per-item results.

Maximum batch size is 100 items per call.

See also: [JSON Envelope Contract](json-envelope.md), [CLI Command Surface](cli/index.md).
