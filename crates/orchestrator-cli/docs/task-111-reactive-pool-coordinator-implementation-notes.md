# TASK-111 Implementation Notes: Reactive Daemon Phase Pool Coordinator

## Phase Context
- Workflow phase: `requirements`
- Workflow ID: `ff5ffe0a-c35c-4182-b1de-ee7e140495e7`
- Task: `TASK-111`

## Purpose
Translate TASK-111 requirements into a deterministic implementation slice that
replaces batch `JoinSet` waiting with completion-driven scheduling and
decoupled housekeeping.

## Chosen Strategy
Adopt a long-lived coordinator loop with three drivers:
1. pool completion stream (`next_completion`),
2. housekeeping timer ticks,
3. `ctrl_c` shutdown signal.

This allows immediate backfill after every completion while preserving existing
workflow/task transition semantics.

## Non-Negotiable Constraints
- No direct edits to `/.ao/*.json`.
- Deterministic behavior for identical queue/workflow snapshots.
- Preserve existing workflow decision handling semantics.
- Keep daemon event schema backward-compatible (additive only).
- Preserve daemon continuity under transient runner failures.

## Proposed Change Surface

### 1) Reactive Main Loop in `daemon_run.rs`
- File: `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_run.rs`
- Replace blocking project tick + sleep control flow with coordinator loop:
  - startup fill,
  - `tokio::select!` on completion/timer/`ctrl_c`,
  - drain-and-exit on shutdown.
- Keep registry sync, daemon lifecycle guards, and notification runtime handling
  intact.

### 2) Scheduler Housekeeping Split
- File: `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_project_tick.rs`
- Extract non-agent operations from monolithic `project_tick` into a dedicated
  housekeeping path callable from timer ticks.
- Keep operation ordering stable:
  1. git outbox flush,
  2. resume/reconcile stale workflow state,
  3. dependency/merge gate reconciliation,
  4. retry failed + promote backlog (when auto-run-ready),
  5. runtime binary refresh checks.

### 3) Completion Processing Extraction
- File: `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_project_tick.rs`
- Replace batch-only `execute_running_workflow_phases_for_project` handling with
  reusable completion processor logic that can run per result event.
- Reuse current result branches:
  - phase complete with decision,
  - research request/rework transitions,
  - manual pending,
  - AI recovery decisions (`retry`, `skip`, `decompose`, `fail`).

### 4) Pool and Work Queue Integration
- Files:
  - `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler.rs`
  - `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_project_tick.rs`
  - TASK-109 pool/channel integration surfaces
- Wire completion stream from TASK-109 channel into coordinator.
- Introduce slot-aware spawn/backfill helpers:
  - acquire next slot from work queue,
  - call `pool.try_spawn(...)`,
  - repeat until no slot or no dispatchable work.

### 5) Tick Summary and Event Emission Compatibility
- Files:
  - `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler.rs`
  - `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_run.rs`
- Ensure summary/event counters remain coherent in reactive mode:
  - executed/failed phase counts,
  - workflow/task transitions,
  - lifecycle/phase execution events.
- Any reactive-specific metadata remains additive.

## Suggested Implementation Sequence
1. Introduce coordinator state model (pool handle, work queue handle, counters).
2. Extract completion-processing helper from current batch execution branch.
3. Extract housekeeping helper from `project_tick`.
4. Replace daemon run loop with `select!` coordinator flow.
5. Wire startup fill and completion backfill logic.
6. Update tests for startup fill, completion-driven backfill, and drain-on-exit.
7. Run targeted runtime daemon tests and fix regressions.

## Testing Plan
- Runtime daemon scheduler tests:
  - completion processing parity with previous behavior,
  - deterministic backfill after completion,
  - housekeeping progression while pool has active runs.
- Runtime daemon run-loop tests:
  - startup pool fill behavior,
  - `ctrl_c` drain behavior,
  - non-fatal error continuation.
- Event compatibility tests:
  - existing event keys preserved,
  - additive metadata parse-safe.

## Validation Targets
- `cargo test -p orchestrator-cli runtime_daemon::daemon_scheduler`
- `cargo test -p orchestrator-cli runtime_daemon::daemon_run`
- `cargo test -p orchestrator-cli runtime_daemon`

## Risks and Mitigations
- Risk: race conditions between completion handling and backfill.
  - Mitigation: single coordinator ownership of slot accounting and spawn calls.
- Risk: housekeeping starvation under high completion volume.
  - Mitigation: timer-driven `select!` branch with bounded per-branch work.
- Risk: transition regression during extraction from batch code paths.
  - Mitigation: keep existing decision handling branches intact and reuse tests.
- Risk: shutdown path dropping completions.
  - Mitigation: explicit drain semantics and deterministic completion flush.

## Deliverables for Next Phase
- Reactive daemon coordinator replacing batch wait behavior.
- Completion-driven scheduling/backfill with stable slot accounting.
- Housekeeping execution decoupled from phase-batch blocking.
- Drain-safe shutdown behavior.
- Updated runtime tests proving reactive semantics and compatibility.
