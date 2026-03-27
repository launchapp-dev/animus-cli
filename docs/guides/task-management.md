# Task Management Guide

Tasks are the primary unit of work in AO. Each task tracks a discrete piece of work from creation through completion, with support for priorities, dependencies, checklists, and agent assignment.

## Creating Tasks

```bash
ao task create --title "Add retry logic to HTTP client" --task-type feature --priority high
```

Available task types:

| Type | Use case |
|------|----------|
| `feature` | New functionality |
| `bugfix` | Fix for a known defect |
| `hotfix` | Urgent production fix |
| `refactor` | Code restructuring without behavior change |
| `docs` | Documentation updates |
| `test` | Test coverage additions |
| `chore` | Maintenance, dependency bumps, CI tweaks |
| `experiment` | Exploratory or spike work |

You can also supply a description inline:

```bash
ao task create \
  --title "Retry HTTP 429 responses" \
  --task-type feature \
  --priority high \
  --description "Implement exponential backoff for rate-limited responses in the HTTP client module."
```

## Task Status Flow

Tasks move through a defined set of statuses:

```
Backlog --> Ready --> In-Progress --> Done
                  \              \
                   \--> Blocked   \--> Cancelled
                   \--> On-Hold
```

Change status with `ao task status`:

```bash
ao task status --id TASK-001 --status ready
ao task status --id TASK-001 --status in-progress
ao task status --id TASK-001 --status done
```

To unblock a task, set it back to `ready`:

```bash
ao task status --id TASK-001 --status ready
```

## Assigning Tasks

Assign a task to an agent with a specific model:

```bash
ao task assign --id TASK-001 --assignee agent:claude --type agent --model claude-sonnet-4-6
```

Or assign to a human:

```bash
ao task assign --id TASK-001 --type human --assignee "alice"
```

## Priority Management

Set priority directly:

```bash
ao task set-priority --id TASK-001 --priority critical
```

Priority levels: `critical`, `high`, `medium`, `low`.

Rebalance priorities across multiple tasks by budget:

```bash
ao task rebalance-priority
```

## Dependencies

Add a dependency so one task blocks another:

```bash
ao task dependency-add --id TASK-002 --dependency-id TASK-001 --type blocks
```

When TASK-001 is not yet done, TASK-002 cannot move to `in-progress`. The daemon respects dependency ordering when picking the next task to execute.

Remove a dependency:

```bash
ao task dependency-remove --id TASK-002 --dependency-id TASK-001
```

## Checklists

Add checklist items to a task for granular tracking:

```bash
ao task checklist-add --id TASK-001 --description "Implement retry logic"
ao task checklist-add --id TASK-001 --description "Add unit tests for backoff"
ao task checklist-add --id TASK-001 --description "Update API docs"
```

Toggle a checklist item as complete:

```bash
ao task checklist-update --id TASK-001 --item-id chk-1 --completed true
```

Agents use checklists during PO review and rework phases to verify acceptance criteria.

## Querying Tasks

List tasks with filters:

```bash
ao task list                             # All tasks
ao task list --status in-progress        # Only in-progress tasks
ao task list --task-type feature         # Only features
ao task list --priority high             # Only high-priority
```

View tasks sorted by priority:

```bash
ao task list --sort priority
```

Get the next task the daemon would pick:

```bash
ao task next
```

View task statistics:

```bash
ao task stats
```

Get a single task by ID:

```bash
ao task get --id TASK-001
```

All commands support `--json` for machine-readable output:

```bash
ao task list --status ready --json
```

## Task History

View workflow dispatch history for a task:

```bash
ao task history --id TASK-001
```

## Pausing and Cancelling

Pause a task (prevents daemon from scheduling it):

```bash
ao task pause --id TASK-001
```

Resume a paused task:

```bash
ao task resume --id TASK-001
```

Cancel a task (requires confirmation):

```bash
ao task cancel --id TASK-001 --confirm TASK-001
```

## Deadlines

Set a deadline:

```bash
ao task set-deadline --id TASK-001 --deadline "2026-03-15T09:30:00Z"
```

Clear a deadline:

```bash
ao task set-deadline --id TASK-001
```
