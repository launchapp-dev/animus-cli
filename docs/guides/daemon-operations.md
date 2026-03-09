# Daemon Operations Guide

The AO daemon is the autonomous scheduler that picks up tasks, dispatches workflows, and manages agent execution. It runs in the background and continuously processes work according to your workflow configuration.

## Starting the Daemon

### Background Mode (Autonomous)

Start the daemon as a detached background process:

```bash
ao daemon start --autonomous
```

This forks a child process and redirects stderr to `.ao/daemon.log`. The daemon will continuously poll for ready tasks and dispatch workflows.

### Foreground Mode

Run the daemon in the foreground for debugging:

```bash
ao daemon run
```

Output streams directly to your terminal. Use Ctrl+C to stop.

## Stopping the Daemon

Graceful shutdown with drain (waits for in-progress phases to complete):

```bash
ao daemon stop
```

## Pausing and Resuming

Pause the scheduler without stopping the daemon process. In-progress work continues but no new work is picked up:

```bash
ao daemon pause
```

Resume scheduling:

```bash
ao daemon resume
```

## Configuration

View and update daemon automation settings:

```bash
ao daemon config
```

Key configuration options:

| Setting | Description |
|---------|-------------|
| `auto_merge` | Automatically merge PRs after successful workflow completion |
| `auto_pr` | Automatically create PRs for completed work |
| `max_workflows` | Maximum concurrent workflows the daemon will run |
| `active_hours` | Time window during which the daemon schedules work |

Update a specific setting:

```bash
ao daemon config --set max_workflows=3
ao daemon config --set auto_merge=true
```

## Monitoring

### Daemon Status

Check whether the daemon is running and its current state:

```bash
ao daemon status
```

### Health Check

Detailed health information including uptime and resource usage:

```bash
ao daemon health
```

### Logs

Read daemon logs:

```bash
ao daemon logs
```

The daemon writes structured JSON log lines to `.ao/daemon.log`. Log rotation occurs at 10MB (rotated file: `.ao/daemon.log.1`).

Clear logs when they grow too large:

```bash
ao daemon clear-logs
```

### Events

Stream the event history to see what the daemon has been doing:

```bash
ao daemon events
```

### Agent Visibility

List agents currently managed by the daemon:

```bash
ao daemon agents
```

## Diagnostics

### Reading Daemon Logs Directly

For real-time debugging, tail the log file:

```bash
tail -f .ao/daemon.log
```

The log contains structured JSON lines with event types like `daemon_startup`, `daemon_shutdown`, workflow dispatches, and phase completions.

### Runner Health

The runner is a separate process from the daemon. It spawns CLI tools (claude, codex, gemini). Check its health:

```bash
ao runner health
```

### Orphan Detection

Detect orphaned runner processes that lost their parent:

```bash
ao runner orphans detect
```

Clean them up:

```bash
ao runner orphans cleanup
```

### Restart Statistics

View how often the runner has restarted:

```bash
ao runner restart-stats
```

## Common Patterns

### Start Daemon and Monitor

```bash
ao daemon start --autonomous
ao daemon status
ao daemon events
```

### Pause While Making Manual Changes

```bash
ao daemon pause
# Make your changes...
ao daemon resume
```

### Debug a Stuck Workflow

```bash
ao daemon status           # Check daemon state
ao daemon logs             # Look for errors
ao runner health           # Check runner process
ao workflow list            # Find the stuck workflow
ao workflow get --id WF-001 # Inspect workflow state
```
