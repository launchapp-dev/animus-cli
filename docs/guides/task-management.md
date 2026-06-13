# Task Management Guide

Tasks are managed through the unified subject surface:
`animus subject ... --kind task`.

## Create a Task

```bash
animus subject create --kind task \
  --title "Add retry logic to HTTP client" \
  --body "Implement exponential backoff for 429 responses." \
  --priority p1 \
  --labels backend,reliability
```

## List and Inspect Tasks

```bash
animus subject list --kind task
animus subject list --kind task --status ready --limit 10
animus subject next --kind task
animus subject get --kind task --id task:TASK-001
```

Subject ids accept either the bare native id (`TASK-001`) or the
kind-qualified form (`task:TASK-001`) wherever an `--id` or subject-id argument
is taken — `subject get/update/status`, `queue hold/release/drop`, and
`workflow run --task-id`. The bare form is normalized to the qualified form at
the CLI boundary, so both resolve the same subject. The human-readable
`animus subject list` table prints the canonical qualified ids.

## Update Task State

```bash
animus subject status --kind task --id task:TASK-001 --status ready
animus subject status --kind task --id task:TASK-001 --status in_progress
animus subject status --kind task --id task:TASK-001 --status done
```

You can also patch priority and labels:

```bash
animus subject update --kind task --id task:TASK-001 --priority p0 --labels urgent,backend
```

## Run a Workflow for a Task

```bash
animus workflow run --task-id TASK-001
```

`animus workflow run --task-id ...` first checks the in-tree task store and
then falls back to the active `subject_backend` resolution path. That means the
same command works for built-in tasks and plugin-owned tasks.

For terminal debugging, use synchronous execution:

```bash
animus workflow run --task-id TASK-001 --sync
```

Built-in tasks usually execute in a managed worktree. Plugin-owned task
subjects execute from `project_root` unless the plugin manages its own checkout
or branching model.

## Queue and Daemon Operations

```bash
animus queue list
animus queue hold task:TASK-001
animus queue release task:TASK-001 task:TASK-002
animus queue drop --all --yes
animus daemon start
```

## Blocked / Paused Ghost State

When a workflow run fails, the daemon calls `apply_task_status` which sets the
task to `Blocked` and also sets `paused: true` on the task record. The scheduler
skips tasks with `paused: true`, so the task will not be auto-dispatched again
even after you fix the underlying problem.

**Symptom:** a task stays in the queue but the daemon never picks it up, even
though `animus subject list` shows it as `blocked` or you have cleared the
blocker manually.

**Cure:** reset the task to `ready`. The `animus subject status` command calls
`apply_task_status` which clears `paused`, `blocked_at`, `blocked_reason`,
`blocked_phase`, and `blocked_by` atomically:

```bash
animus subject status --kind task --id task:TASK-001 --status ready
```

Never edit the task JSON directly — only the command surface clears all the
associated fields correctly.

## Notes

- The legacy `animus task ...` command tree was removed.
- Task creation and status transitions now route through the active
  `subject_backend` for `kind=task`.
- Workflow history is tracked through `animus workflow list` and
  `animus history ...`, not a dedicated `task history` command.
