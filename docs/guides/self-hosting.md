# Self-Hosting Workflow

AO is built using AO. The project's own requirements and tasks are tracked through the same `ao` commands that users run on their projects. This guide documents that workflow.

## The "AO Builds AO" Loop

The development cycle follows this pattern:

1. Requirements are drafted and refined using `ao requirements`
2. Tasks are created from requirements using `ao requirements execute`
3. The daemon picks up tasks and dispatches workflows
4. Agents implement, test, and review changes
5. Completed work is merged back into the codebase

## Viewing the Backlog

List all requirements:

```bash
ao requirements list
```

View prioritized tasks:

```bash
ao task prioritized
```

Check task statistics for overall progress:

```bash
ao task stats
```

## Working on a Task

Get the next highest-priority ready task:

```bash
ao task next
```

Start work on it:

```bash
ao task status --id TASK-XXX --status in-progress
```

Complete the task:

```bash
ao task status --id TASK-XXX --status done
```

## Autonomous Execution

For fully autonomous operation, start the daemon:

```bash
ao daemon start --autonomous
```

The daemon will:

1. Poll for ready tasks
2. Respect dependency ordering
3. Create git worktrees for each task
4. Dispatch the configured workflow pipeline
5. Run through phases: requirements, implementation, testing, review
6. Create PRs and optionally auto-merge on success
7. Clean up worktrees after merge

Monitor progress:

```bash
ao daemon status
ao daemon events
ao task stats
```

## Task State Management

When the daemon encounters issues, tasks may end up in a blocked state. Always use `ao task status` to reset:

```bash
ao task status --id TASK-XXX --status ready
```

This clears all blocking metadata (`paused`, `blocked_at`, `blocked_reason`, `blocked_by`). Never edit task JSON files in `.ao/` directly.

## Environment Setup

When running the daemon from inside a Claude Code session, the `CLAUDECODE` environment variable is inherited. This prevents the `claude` CLI from starting. Unset it before launching:

```bash
env -u CLAUDECODE ao daemon start --autonomous
```

Or:

```bash
unset CLAUDECODE
ao daemon start --autonomous
```
