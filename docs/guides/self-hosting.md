# Self-Hosting Workflow

Animus can be used to operate an Animus-managed repository through the same
CLI and daemon surfaces it exposes to other projects.

## The Loop

1. Create or inspect requirements with `animus subject --kind requirement`.
2. Create or inspect tasks with `animus subject --kind task`.
3. Mark work ready through `animus subject status`.
4. Start the daemon or run a workflow directly.
5. Monitor workflows, queue state, logs, and outputs.

## Backlog Inspection

```bash
animus subject list --kind requirement
animus subject list --kind task --status ready
animus subject next --kind task
animus workflow list
animus status
```

## Working a Task

```bash
animus subject status --kind task --id task:TASK-001 --status in_progress
animus workflow run --subject-id TASK-001 --sync
animus subject status --kind task --id task:TASK-001 --status done
```

## Autonomous Execution

```bash
animus daemon start
animus daemon status
animus daemon health
animus daemon observe
animus logs tail --limit 100
```

## Operational Notes

- Use `animus subject status --kind task --id <task-id> --status ready` to
  clear a blocked task back into the ready pool.
- Never hand-edit Animus-managed state files.
- If you launch the daemon from inside a Claude Code session, unset
  `CLAUDECODE` first if the provider CLI depends on a clean environment.
