# Internals Overview

This section documents the internal mechanisms of AO for contributors who want to understand how the system works beneath the CLI surface.

## What's Covered

- [Daemon Scheduler](daemon-scheduler.md) -- The tick loop that drives autonomous workflow dispatch, capacity management, and completion reconciliation
- [Workflow Runner](workflow-runner.md) -- The standalone binary that executes workflow phases by coordinating with the agent runner
- [Agent Runner IPC](agent-runner-ipc.md) -- The IPC protocol between workflow-runner and agent-runner, including authentication, event streaming, and output parsing
- [State Machines](state-machines.md) -- Workflow and task state machines, transition rules, and guard conditions
- [Persistence](persistence.md) -- Atomic file writes, JSON state schemas, and the scoped directory layout

## Key Concepts

**Tick loop**: The daemon operates on a periodic tick. Each tick loads state, plans dispatches, reconciles completions, and spawns new workflow-runner subprocesses.

**Subject dispatch**: Every workflow execution targets a "subject" (typically a task). The dispatch queue orders subjects by priority and tracks their lifecycle from enqueued through assigned to terminal.

**Three-process model**: The daemon spawns `ao-workflow-runner` processes, which in turn communicate with the `ao-agent-runner` daemon over IPC. The agent runner manages the actual LLM CLI tool processes (claude, codex, gemini, opencode).

```
ao daemon (tick loop)
  └── ao-workflow-runner (phase execution)
        └── ao-agent-runner (LLM CLI management)
              └── claude / codex / gemini / opencode
```

## Related Sections

- [Architecture Overview](../architecture/index.md) -- Crate dependency graph and high-level design
- [ServiceHub Pattern](../architecture/service-hub.md) -- Dependency injection
- [Crate Map](../architecture/crate-map.md) -- All crates by responsibility
