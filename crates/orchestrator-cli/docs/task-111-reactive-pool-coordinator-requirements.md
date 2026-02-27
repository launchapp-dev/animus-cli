# TASK-111 Requirements: Replace JoinSet Batch Wait with Reactive Pool Coordinator

## Phase
- Workflow phase: `requirements`
- Workflow ID: `ff5ffe0a-c35c-4182-b1de-ee7e140495e7`
- Task: `TASK-111`

## Objective
Eliminate batch-style daemon phase execution so workflow progress and scheduling
react to each agent completion immediately.

Target behavior:
- startup fills the execution pool to capacity from the current work queue,
- each completion is processed individually and immediately triggers backfill,
- housekeeping runs on timer ticks without waiting for all in-flight phases,
- `ctrl_c` drains in-flight work before daemon loop exit.

## Current Baseline Audit
Snapshot date: `2026-02-27`.

| Surface | Current location | Current behavior | Gap |
| --- | --- | --- | --- |
| Daemon outer loop | `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_run.rs` (`handle_daemon_run`) | iterates `project_tick(...)` then sleeps by interval | project tick can block for long-running phase batches |
| Tick orchestration | `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_project_tick.rs` (`project_tick`) | performs housekeeping, ready-start, binary refresh, and phase execution in one pass | no event-driven backfill between phase completions |
| Phase execution | `daemon_scheduler_project_tick.rs` (`execute_running_workflow_phases_for_project`) | schedules several phase runs, waits through `JoinSet::join_next()` until all scheduled runs settle | batch wait introduces head-of-line blocking |
| Completion handling | `execute_running_workflow_phases_for_project` result branch | completion/failure updates happen after join result pull | no immediate slot refill on each completion |
| Housekeeping cadence | `project_tick` fixed-order execution | housekeeping and phase execution are coupled to tick cycle | housekeeping can be delayed by long in-flight phase execution |

## Problem Statement
The daemon currently uses a batch phase execution model that waits on a
`JoinSet` loop for scheduled runs. This delays backfill and housekeeping,
reducing scheduler responsiveness and effective parallelism under mixed-duration
phase workloads.

## Scope
In scope for implementation after this phase:
- Replace batch phase waiting with a reactive coordinator loop driven by:
  - `pool.next_completion()`,
  - housekeeping timer ticks,
  - `ctrl_c`.
- Introduce deterministic startup pool fill from a work queue.
- Process each completion with workflow/task state updates and immediate
  slot-based backfill.
- Move non-agent scheduler operations behind a dedicated housekeeping path.
- Preserve additive daemon event semantics while reflecting reactive progress.
- Add targeted regression tests for completion-driven scheduling and graceful
  shutdown.

Out of scope:
- Redesign of workflow phase semantics or decision schema.
- Manual edits to `/.ao/*.json`.
- New desktop-wrapper dependencies.
- Non-deterministic/adaptive queue heuristics.

## Constraints
- Scheduler behavior must be deterministic for identical workflow/task snapshots.
- Existing task/workflow mutation paths must remain service/API-driven.
- Daemon loop continuity must remain resilient to transient runner errors.
- Existing JSON envelope/event shape remains backward-compatible (additive only).
- Pool capacity must respect current daemon/task caps (`max_tasks_per_tick` and
  effective agent capacity constraints).

## Reactive Coordinator Contract

### Loop Shape
Coordinator loop must follow:
1. `result = pool.next_completion()` -> process completion and backfill.
2. `housekeeping_timer.tick()` -> run housekeeping operations.
3. `ctrl_c` -> drain pool and exit loop.

### Completion Path
For every completion event:
1. Run completion result handling equivalent to existing phase result logic
   (`complete_current_phase_with_decision`, `request_research`,
   `fail_current_phase`, AI recovery decisions, and task status sync).
2. Pull next available slot from work queue.
3. Attempt spawn via `pool.try_spawn(...)` while slots and eligible work remain.

### Housekeeping Path
Housekeeping tick runs non-agent operations only:
- `flush_git_integration_outbox`,
- interrupted workflow resume/stale cleanup path,
- stale/dependency/merge gate reconciliation,
- runtime binary refresh check,
- backlog promotion,
- failed-workflow retry.

### Startup and Shutdown
- Startup: prefill pool from work queue up to capacity before waiting.
- Shutdown: on `ctrl_c`, `pool.drain()` awaited before daemon exits.

## Functional Requirements

### FR-01: Startup Pool Fill
On coordinator startup, daemon computes available slots and fills the pool from
the work queue until no slot or no eligible workflow phase remains.

### FR-02: Completion-Driven Backfill
Each agent completion is processed immediately; backfill is attempted in the
same iteration without waiting for other in-flight runs.

### FR-03: Housekeeping Isolation
Housekeeping operations execute on timer ticks and are no longer blocked behind
batch phase joins.

### FR-04: Batch Join Elimination
The batch `JoinSet` wait model in phase execution is replaced by the reactive
completion stream from pool/channel integration.

### FR-05: Deterministic Slot Accounting
Spawn/backfill decisions use deterministic slot accounting and queue order for
identical state inputs.

### FR-06: Graceful Ctrl-C Exit
`ctrl_c` initiates deterministic drain of in-flight pool work, then exits
without abandoning tracked completion processing.

### FR-07: Error Containment
Transient runner failures remain non-fatal to coordinator progress and do not
crash daemon loop execution.

### FR-08: Event Compatibility
Daemon events remain parse-compatible for existing consumers, with any new
reactive metadata added additively.

### FR-09: Operational Compatibility
Existing ready-task workflow startup and task/workflow status transitions remain
semantically equivalent from an operator perspective.

### FR-10: Regression Coverage
Add/adjust tests to cover startup fill, per-completion backfill, housekeeping
tick behavior, and drain-on-exit semantics.

## Acceptance Criteria
- `AC-01`: Phase execution no longer waits for a full batch before scheduling
  additional work.
- `AC-02`: A completion event triggers completion handling and immediate
  backfill attempt in the same coordinator cycle.
- `AC-03`: Housekeeping operations continue to run on timer ticks while agent
  work is in flight.
- `AC-04`: Startup fills pool to available capacity from queued eligible work.
- `AC-05`: `ctrl_c` drains in-flight pool work before daemon exits.
- `AC-06`: Existing workflow/task transition semantics remain valid after
  reactive refactor.
- `AC-07`: Daemon event output remains backward-compatible.
- `AC-08`: Targeted runtime tests pass for reactive scheduling behavior.

## Testable Acceptance Checklist
- `T-01`: unit/integration test for startup pool prefill to capacity.
- `T-02`: test verifies one completion causes immediate backfill attempt.
- `T-03`: test verifies housekeeping timer path executes while pool has active
  work.
- `T-04`: test verifies transient completion errors do not terminate loop.
- `T-05`: test verifies `ctrl_c` path drains pool before exit.
- `T-06`: test verifies task/workflow state sync remains consistent with prior
  semantics.
- `T-07`: daemon event test verifies additive compatibility.

## Verification Matrix
| Requirement area | Verification method |
| --- | --- |
| FR-01, FR-02, FR-05 | scheduler/runtime tests around pool fill and completion-driven backfill |
| FR-03 | housekeeping cadence tests with in-flight phase simulation |
| FR-04 | code-path audit removing batch `JoinSet` wait model from active execution path |
| FR-06 | daemon-run shutdown/drain tests |
| FR-07, FR-09 | workflow/task transition regression tests |
| FR-08 | daemon event payload compatibility assertions |
| FR-10 | targeted `cargo test -p orchestrator-cli runtime_daemon` suites |

## Implementation Notes Input (Next Phase)
Primary expected change targets:
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_run.rs`
  - replace blocking tick/sleep loop with reactive coordinator/select loop.
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler.rs`
  - expose coordinator-facing summary structs/helpers where needed.
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_project_tick.rs`
  - split batch phase execution into:
    - completion processing helper,
    - housekeeping helper,
    - queue/slot integration hooks.

Likely supporting surfaces:
- pool/channel primitives introduced in TASK-109 integration points.
- daemon runtime tests under
  `crates/orchestrator-cli/src/services/runtime/runtime_daemon/`.

## Deterministic Deliverables for Implementation Phase
- Reactive coordinator loop with completion-driven scheduling.
- Startup prefill and completion-triggered backfill semantics.
- Housekeeping execution decoupled from batch phase waits.
- Graceful drain-on-exit behavior.
- Regression tests covering reactive scheduling correctness.
