# Daemon Operations Guide

The Animus daemon is the autonomous scheduler that picks up tasks, dispatches workflows, and manages agent execution. It runs in the background and continuously processes work according to your workflow configuration.

## Starting the Daemon

### Background Mode (Default)

Start the daemon as a detached background process (as of v0.6 `daemon start`
always detaches; the legacy `--autonomous` flag is a deprecated no-op):

```bash
animus daemon start
```

This forks a child process and continuously polls for ready work. The
command prints the daemon pid and the background log path. Structured
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
| `pool_size` | Maximum number of concurrent agents the daemon will run |
| `active_hours` | Time window during which schedule-driven workflow dispatch is allowed |
| `auto_run_ready` | Whether ready tasks are promoted during daemon ticks |

`active_hours` only gates schedule-driven dispatch. Ready-task pickup is controlled separately by `auto_run_ready`.

Update a specific setting:

```bash
animus daemon config --pool-size 3
animus daemon config --auto-run-ready false
```

## Monitoring

### Daemon Status

Check whether the daemon is running and its current state:

```bash
animus daemon status
```

`animus daemon status` reads the persisted daemon state and reports whether at
least one installed `animus-provider-*` binary is executable. It does not spawn
or manifest-probe every installed plugin just to answer the health question.

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

Print recent event history to see what the daemon has been doing:

```bash
animus daemon events --limit 50
```

The command prints the requested batch and exits. Pass `--follow` to keep
streaming new events until interrupted.

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

### Provider Plugin Health

Provider plugins spawn the CLI tools (claude, codex, gemini). Check their health:

```bash
animus plugin status
```

This reports per-plugin runtime state plus a `providers` array (one row per
discovered provider plugin) and a summary `provider_plugins_healthy` flag. A
provider only counts as installed when its binary exists and is executable, so
a lost execute bit shows up here before the next agent run fails at spawn time.

### Orphan Detection

Detect orphaned CLI processes that lost their parent:

```bash
animus doctor --check orphan_cli_processes
```

Clean them up:

```bash
animus doctor --fix
```

`--fix` prunes tracker entries whose process already exited. Live tracked
PIDs are never killed automatically — the cli-tracker is global across
projects, so the check prints a manual `kill <pid>` suggestion and leaves
the decision to you.

## Common Patterns

### Start Daemon and Monitor

```bash
animus daemon start
animus daemon status
animus daemon events --follow
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
animus plugin status           # Check plugin + provider health
animus workflow list            # Find the stuck workflow
animus workflow get --id WF-001 # Inspect workflow state
```
