# The Daemon

## Dumb Scheduler, Not a Feature Host

The Animus daemon is a scheduler. It consumes [SubjectDispatch](./subject-dispatch.md) envelopes, manages subprocess capacity, spawns `workflow-runner` processes, and emits execution facts. It does not contain AI logic, task policy, or business rules.

This deliberate simplicity keeps the daemon generic. Advanced behavior lives in [YAML workflows](./workflows.md) executed by `workflow-runner`.

---

## The Tick Loop

The daemon operates on a periodic tick (default: every 5 seconds). Each tick follows the same sequence:

```mermaid
flowchart TB
    subgraph TICK["Daemon Tick"]
        load["1. Load dispatch queue"]
        check["2. Check capacity"]
        dequeue["3. Dequeue highest-priority SubjectDispatch"]
        spawn["4. Spawn workflow-runner with workflow_ref + subject"]
        poll["5. Poll running subprocesses"]
        emit["6. Emit execution facts"]
    end

    load --> check --> dequeue --> spawn --> poll --> emit
    emit -->|"next tick"| load
```

### Step by step

1. **Load dispatch queue** -- Read queued `SubjectDispatch` values, ordered by priority and `requested_at`.
2. **Check capacity** -- Determine how many new workflows can be started given the current slot usage and headroom configuration.
3. **Dequeue** -- Pop the highest-priority dispatch that fits within capacity.
4. **Spawn** -- Start a `workflow-runner` subprocess, passing the `workflow_ref`, subject identity, and input. Built-in task subjects get a managed [worktree](./worktrees.md); plugin-owned task subjects run from `project_root` unless the plugin provides its own checkout strategy.
5. **Poll** -- Check all active `workflow-runner` subprocesses for completion, telemetry, or failure.
6. **Emit facts** -- Publish execution facts (workflow started, phase completed, workflow succeeded/failed) for projectors to consume.

---

## The Wake Model

The main loop is event-driven rather than busy-polling. It sleeps until
something asks it to wake: a `daemon/nudge` control message (sent
fire-and-forget by `animus subject create/update/status` and
`animus queue enqueue/release`), a workflow or phase completion event, a config
hot-reload, or a precise cron deadline computed from compiled `schedules:`. The
`interval_secs` heartbeat is only a fallback that bounds out-of-band pickup and
paces housekeeping — it is not the primary driver.

```mermaid
flowchart TB
    sleep["Daemon main loop<br/>sleeps until woken"]
    nudge["daemon/nudge<br/>(subject + queue mutations)"]
    completion["workflow / phase<br/>completion event"]
    reload["config hot-reload"]
    cron["cron deadline<br/>(compiled schedules:)"]
    heartbeat["interval_secs<br/>(fallback heartbeat)"]
    work["Run scheduler leg:<br/>lease + dispatch, reconcile"]

    nudge --> work
    completion --> work
    reload --> work
    cron --> work
    heartbeat --> work
    work --> sleep
    sleep -.-> nudge
    sleep -.-> completion
    sleep -.-> reload
    sleep -.-> cron
    sleep -.-> heartbeat
```

A single nudge from enqueueing a subject flows all the way to a spawned
workflow run without waiting for the next heartbeat:

```mermaid
sequenceDiagram
    participant CLI as animus queue enqueue
    participant Q as Dispatch queue
    participant D as Daemon main loop
    participant R as workflow-runner

    CLI->>Q: persist SubjectDispatch
    CLI->>D: daemon/nudge (fire-and-forget)
    D->>D: wake, check capacity
    D->>Q: lease highest-priority dispatch
    Q-->>D: SubjectDispatch (workflow_ref + subject)
    D->>R: spawn with workflow_ref + subject
    R-->>D: execution facts (started / completed)
    D->>D: back to sleep
```

---

## Capacity Management

The daemon controls concurrency through three mechanisms:

| Control | Description |
|---------|-------------|
| **Max concurrent workflows** | Hard limit on how many `workflow-runner` subprocesses can run simultaneously. |
| **Slot headroom** | Reserve slots so the system is never fully saturated. Allows high-priority work to preempt. |
| **Priority ordering** | Dispatches are dequeued in priority order. Within the same priority, earlier `requested_at` wins. |

The daemon tracks active subjects to prevent duplicate dispatches for the same subject.

---

## What the Daemon Knows vs. Does Not Know

### Knows about

- **Subjects** -- generic `SubjectRef` identity (`kind`, `id`, optional metadata).
- **Dispatch envelopes** -- `SubjectDispatch` with `workflow_ref`, priority, trigger source.
- **Slots and headroom** -- How many workflows are running, how many can start.
- **Subprocess lifecycle** -- PID tracking, health checks, orphan detection on restart.
- **Runner telemetry** -- Phase progress, resource usage, timing.
- **Workflow execution events** -- Started, phase completed, succeeded, failed.

### Does NOT know about

- Task status policy (backlog, ready, blocked transitions).
- Backlog promotion rules.
- Retry policy (handled by workflow-runner's rework loop).
- Requirement state transitions.
- AI logic, prompts, or model selection.
- Git workflow policy (branching, merging, PR creation).

These responsibilities belong to [workflow-runner](./workflows.md), [projectors](#execution-facts-and-projectors), or [MCP tool surfaces](./mcp-tools.md).

---

## Execution Facts and Projectors

When a workflow completes (or fails), the daemon emits execution facts. Projectors subscribe to these facts and update domain state accordingly.

```mermaid
flowchart LR
    runner["workflow-runner<br/>completes"]
    fact["Execution Fact<br/>subject + outcome + metadata"]

    subgraph PROJECTORS["Projectors"]
        tp["Task Projector<br/>TASK-001 status: done"]
        rp["Requirement Projector<br/>REQ-003 tasks created"]
        sp["Schedule Projector<br/>Record run history"]
        np["Notification Projector<br/>Alert on failure"]
    end

    runner --> fact
    fact --> tp
    fact --> rp
    fact --> sp
    fact --> np
```

The daemon emits facts; it never interprets them. This separation means adding a new projector (e.g. a Slack notifier) does not require changing the daemon.

---

## Starting and Stopping

```bash
animus daemon start    # Start daemon in background (forks child process)
animus daemon status                # Check daemon health and active workflows
animus daemon pause                 # Pause dispatch (running workflows continue)
animus daemon resume                # Resume dispatch
animus daemon stop                  # Graceful shutdown
```

`animus daemon start` always forks a detached child process (use
`animus daemon run` to stay in the foreground for debugging). Structured
runtime events are written through the active log storage backend and mirrored
locally at `~/.animus/<repo-scope>/logs/events.jsonl`. Use `animus daemon stream`
for live events and `animus logs tail` for recent persisted entries.

For provider readiness, `animus daemon status` only needs one installed,
executable `animus-provider-*` binary to count providers as healthy. It does
not spawn or manifest-probe unrelated plugins during that check.

### Failure Recovery

- **Daemon crashes** -- On next startup, orphan recovery detects and cleans up
  stale subprocesses, but newly-started workflows get a 90-second grace window
  before they can be treated as orphaned.
- **workflow-runner crashes** -- The daemon detects the process exit and emits a failure fact.
- **Phase fails inside a workflow** -- Handled by workflow-runner's [rework loop](./agents-and-phases.md), not by the daemon.
