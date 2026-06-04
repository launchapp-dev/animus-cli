# Daemon Operations Guide

The Animus daemon is the autonomous scheduler that picks up tasks, dispatches workflows, and manages agent execution. It runs in the background and continuously processes work according to your workflow configuration.

## Starting the Daemon

### Background Mode (Autonomous)

Start the daemon as a detached background process:

```bash
animus daemon start --autonomous
```

This forks a child process and continuously polls for ready work. Structured
runtime events are persisted through the active log storage backend, and the
scoped local mirror remains `~/.animus/<repo-scope>/logs/events.jsonl`.

### Foreground Mode

Run the daemon in the foreground for debugging:

```bash
animus daemon run
```

Output streams directly to your terminal. Use Ctrl+C to stop.

## Stopping the Daemon

Graceful shutdown with drain. This waits for in-progress phases to complete and
gives installed notifier plugins a final drain window so the terminal
`status: stopped` / shutdown events are delivered before the daemon process
exits:

```bash
animus daemon stop
```

## Pausing and Resuming

Pause the scheduler without stopping the daemon process. In-progress work continues but no new work is picked up:

```bash
animus daemon pause
```

Resume scheduling:

```bash
animus daemon resume
```

## Configuration

View and update daemon automation settings:

```bash
animus daemon config
```

Key configuration options:

| Setting | Description |
|---------|-------------|
| `auto_merge` | Automatically merge PRs after successful workflow completion |
| `auto_pr` | Automatically create PRs for completed work |
| `pool_size` | Maximum number of concurrent agents the daemon will run |
| `active_hours` | Time window during which schedule-driven workflow dispatch is allowed |
| `auto_run_ready` | Whether ready tasks are promoted during daemon ticks |

`active_hours` only gates schedule-driven dispatch. Ready-task pickup is controlled separately by `auto_run_ready`.

Update a specific setting:

```bash
animus daemon config --pool-size 3
animus daemon config --auto-merge true
animus daemon config --auto-run-ready false
```

## Monitoring

### Daemon Status

Check whether the daemon is running and its current state:

```bash
animus daemon status
```

### Health Check

Detailed health information including uptime and resource usage:

```bash
animus daemon health
```

### Logs

Read daemon logs:

```bash
animus daemon logs
```

The daemon writes structured log entries through the active log storage
backend. Redacted JSON lines are also persisted under
`~/.animus/<repo-scope>/logs/events.jsonl`, which remains the local mirror
for daemon events.

Daemon automation settings, including `notification_config`, are persisted
separately under `~/.animus/<repo-scope>/daemon/pm-config.json`.

Clear logs when they grow too large:

```bash
animus daemon clear-logs
```

### Events

Stream the event history to see what the daemon has been doing:

```bash
animus daemon events
```

### Agent Visibility

List agents currently managed by the daemon:

```bash
animus daemon agents
```

## Diagnostics

### Reading Daemon Logs Directly

For recent persisted entries, use the log tail command:

```bash
animus logs tail --limit 100
```

`animus logs tail` reads through the active `log_storage_backend` when one is
installed, and otherwise reads the local `events.jsonl` mirror directly. Its
`--follow` flag is currently reserved for future backend streaming support, so
the local file path still returns a batch and exits.

For live debugging, stream daemon events:

```bash
animus daemon stream --pretty
```

The stream contains structured events like `daemon_startup`, `daemon_shutdown`,
workflow dispatches, and phase completions.

### Runner Health

The runner is a separate process from the daemon. It spawns CLI tools (claude, codex, gemini). Check its health:

```bash
animus runner health
```

### Orphan Detection

Detect orphaned runner processes that lost their parent:

```bash
animus runner orphans detect
```

Clean them up:

```bash
animus runner orphans cleanup
```

### Restart Statistics

View how often the runner has restarted:

```bash
animus runner restart-stats
```

## Common Patterns

### Start Daemon and Monitor

```bash
animus daemon start --autonomous
animus daemon status
animus daemon events
```

### Pause While Making Manual Changes

```bash
animus daemon pause
# Make your changes...
animus daemon resume
```

### Debug a Stuck Workflow

```bash
animus daemon status           # Check daemon state
animus daemon logs             # Look for errors
animus runner health           # Check runner process
animus workflow list            # Find the stuck workflow
animus workflow get --id WF-001 # Inspect workflow state
```
