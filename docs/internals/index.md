# Internals Overview

This section documents the internal mechanisms of Animus for contributors who want to understand how the system works beneath the CLI surface.

## What's Covered

- [Daemon Scheduler](daemon-scheduler.md) -- The tick loop that drives autonomous workflow dispatch, capacity management, and completion reconciliation
- [Workflow Runner](workflow-runner.md) -- The standalone workflow-runner plugin path that executes phases and streams results back to Animus
- [Provider Sessions](agent-runner-ipc.md) -- The current provider-plugin session path used by `animus agent` and workflow agent phases
- [State Machines](state-machines.md) -- Workflow and task state machines, transition rules, and guard conditions
- [Persistence](persistence.md) -- Atomic file writes, JSON state schemas, and the scoped directory layout

## Key Concepts

**Tick loop**: The daemon is event-driven. It wakes on `daemon/nudge` control messages, workflow/phase completion events, config hot-reloads, and cron deadlines, with `interval_secs` as a fallback heartbeat. Each tick loads state, plans dispatches, reconciles completions, and spawns new workflow-runner subprocesses.

**Subject dispatch**: Every workflow execution targets a "subject" (typically a task). The dispatch queue orders subjects by priority and tracks their lifecycle from enqueued through assigned to terminal.

**Current execution model**: The daemon launches an installed `workflow_runner`
plugin for phase execution. Agent phases and `animus agent` commands resolve a
provider plugin through `orchestrator-plugin-host::session`, which then drives
the actual CLI/tool integration.

```
animus daemon (tick loop)
  └── workflow_runner plugin (phase execution)
        └── orchestrator-plugin-host::session
              └── provider plugin
                    └── claude / codex / gemini / opencode / oai
```

## Related Sections

- [Architecture Overview](../architecture/index.md) -- Crate dependency graph and high-level design
- [ServiceHub Pattern](../architecture/service-hub.md) -- Dependency injection
- [Crate Map](../architecture/crate-map.md) -- All crates by responsibility
