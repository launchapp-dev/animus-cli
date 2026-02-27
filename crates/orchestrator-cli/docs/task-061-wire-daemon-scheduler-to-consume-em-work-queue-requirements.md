# TASK-061 Requirements: Wire Daemon Scheduler to Consume EM Work Queue

## Phase
- Workflow phase: `requirements`
- Workflow ID: `7d5c75c6-bf4f-4a73-8a1f-ecfdf51591ad`
- Task: `TASK-061`

## Objective
Make daemon ready-task scheduling deterministic with EM-directed ordering:
- prefer EM work queue entries when queue data exists and has dispatchable work,
- preserve current priority picker as fallback when queue has no dispatchable entries,
- track queue entry lifecycle (`pending -> assigned -> removed`) across workflow start
  and terminal workflow outcomes,
- emit explicit per-task selection source telemetry (`em_queue` or
  `fallback_picker`).

## Current Baseline Audit
Snapshot date: `2026-02-27`.

| Surface | Current location | Current behavior | Gap |
| --- | --- | --- | --- |
| Ready-task workflow startup | `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_project_tick.rs` (`run_ready_task_workflows_for_project`) | always enumerates `tasks.list_prioritized()` and starts eligible `ready` tasks | no EM queue precedence path |
| Daemon concurrency signal | `crates/orchestrator-core/src/services/daemon_impl.rs` (`health`) | provides `active_agents` and optional `max_agents` | ready-task startup path does not consume this signal when deciding batch size |
| Workflow terminal reconciliation | `daemon_scheduler_project_tick.rs` (`sync_task_status_for_workflow_result`) | syncs task status with workflow terminal states | no queue-entry lifecycle cleanup hook |
| Daemon task-state telemetry | `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_run.rs` (`task-state-change` events) | emits status transitions with task/workflow/phase fields | no task selection-source field |
| Repo-scoped daemon runtime state | `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_git_ops.rs` (`repo_ao_root`) | supports deterministic repo-scoped state under `~/.ao/<repo-scope>/...` | no documented EM queue persistence/loader contract in scheduler runtime |

## Problem Statement
The scheduler currently ignores EM-curated task ordering and only uses the
priority-based picker. This prevents explicit EM queue sequencing, does not
track queue assignment/removal lifecycle, and leaves no machine-readable signal
for whether a started task came from EM queue or fallback picker.

## Scope
In scope for implementation after this phase:
- Add queue-aware task selection in daemon ready-task startup.
- Define deterministic dispatch-limit calculation that respects scheduler tick
  cap and daemon agent-capacity constraints.
- Apply queue entry lifecycle updates:
  - mark selected queue entries as `assigned` after successful workflow start,
  - remove assigned entries when workflow reaches terminal completion/failure.
- Preserve existing fallback picker for queue-miss/empty cases.
- Emit per-task source telemetry for starts (`em_queue` vs `fallback_picker`).
- Add targeted scheduler/event tests for precedence and lifecycle semantics.

Out of scope:
- Redesigning task priority policy or fallback picker ranking.
- Broad workflow state-machine refactors.
- Manual edits to `/.ao/*.json`.
- New desktop-wrapper dependencies.

## Constraints
- Queue consumption must be deterministic and stable for identical input state.
- Existing `run_ready_task_workflows_for_project` eligibility checks remain
  intact (paused/cancelled/status/dependency/active/completed guards).
- Queue behavior must not break daemon loop continuity; queue load/parse errors
  degrade safely to fallback picker.
- Existing daemon event payloads remain additive-compatible (no removal/rename
  of existing fields).
- State mutation path remains service/API-driven and repo-safe.

## EM Queue Dispatch Contract

### Queue Entry Lifecycle
- `pending`: entry eligible for scheduler selection.
- `assigned`: workflow started for entry; entry remains until workflow reaches
  terminal completion/failure.
- `removed`: entry deleted after terminal workflow result processing.

### Dispatch Limit
Scheduler dispatch limit per tick is:
1. `tick_cap = args.max_tasks_per_tick`
2. `agent_cap = daemon.health.max_agents` (when set)
3. `active_agents = daemon.health.active_agents`
4. `available_agent_slots = max(agent_cap - active_agents, 0)` (when `agent_cap`
   exists)
5. `dispatch_limit = min(tick_cap, available_agent_slots)` when `agent_cap`
   exists, else `tick_cap`

### Source Selection Rules
- Use queue-first selection when queue exists and has at least one `pending`
  dispatchable entry.
- Use fallback priority picker only when queue is unavailable or has no
  dispatchable `pending` entries.
- Record source on each workflow start:
  - `em_queue` for queue-selected entries,
  - `fallback_picker` for priority-list fallback selections.

## Functional Requirements

### FR-01: Queue-First Task Selection
When EM queue exists and has dispatchable `pending` entries, scheduler must
attempt queue entries before using fallback picker.

### FR-02: Deterministic Batch Extraction
Scheduler must consume at most `dispatch_limit` queue-selected tasks per tick
using queue order as stored by EM queue.

### FR-03: Eligibility Parity
Queue-selected tasks must pass the same readiness gates as fallback-selected
tasks:
- status `ready`,
- not paused/cancelled,
- no active workflow for the task,
- not already terminally completed via workflow list,
- dependency gate checks pass.

### FR-04: Assign Entry on Successful Workflow Start
After `workflows.run(...)` succeeds for a queue-selected task, the
corresponding queue entry is marked `assigned` (including associated
`workflow_id` where available).

### FR-05: Remove Entry on Terminal Workflow Result
When an assigned queue entry's workflow reaches terminal completion/failure,
remove that queue entry from EM queue state.

### FR-06: Fallback Compatibility
If queue is unavailable, empty, or has no dispatchable `pending` entries for
the current tick, scheduler falls back to existing priority picker behavior.

### FR-07: Source Telemetry
Emit task-level source selection telemetry so operators and MCP consumers can
determine whether each started workflow came from `em_queue` or
`fallback_picker`.

### FR-08: Non-Fatal Queue Errors
Queue decode/load/update failures must not crash scheduler tick; failures are
reported via daemon diagnostics and fallback picker remains available.

### FR-09: Backward Compatibility
Existing daemon queue/health/workflow/task-state event fields remain valid;
source telemetry is additive.

### FR-10: Regression Coverage
Add tests for queue precedence, fallback behavior, assignment/removal lifecycle,
and source telemetry payload.

## Acceptance Criteria
- `AC-01`: With queue containing dispatchable `pending` entries, ready-task
  startup uses queue order before fallback picker.
- `AC-02`: Started task count in a tick never exceeds calculated
  `dispatch_limit`.
- `AC-03`: Queue-selected task is marked `assigned` only after successful
  workflow start.
- `AC-04`: Assigned queue entry is removed when workflow is terminally
  completed or failed.
- `AC-05`: When queue has no dispatchable pending entries, scheduler falls back
  to current priority picker.
- `AC-06`: Per-task selection source is emitted as `em_queue` or
  `fallback_picker`.
- `AC-07`: Queue load/update failures are non-fatal and preserve daemon tick
  continuity.
- `AC-08`: Existing daemon event consumers are not broken by the change.

## Testable Acceptance Checklist
- `T-01`: scheduler test where queue has ready entries and start order follows
  queue, not `list_prioritized()` order.
- `T-02`: scheduler test verifies dispatch count obeys `min(max_tasks_per_tick,
  max_agents - active_agents)` when agent cap exists.
- `T-03`: scheduler test verifies queue-selected workflow start marks entry
  `assigned`.
- `T-04`: scheduler test verifies terminal workflow outcome removes assigned
  queue entry.
- `T-05`: scheduler test verifies empty/non-dispatchable queue path falls back
  to priority picker.
- `T-06`: daemon event test verifies source telemetry field/value for started
  tasks.
- `T-07`: scheduler test verifies queue load/update failure degrades safely to
  fallback with no tick crash.

## Verification Matrix

| Requirement area | Verification method |
| --- | --- |
| FR-01, FR-02, FR-03 | daemon scheduler unit/integration tests around `run_ready_task_workflows_for_project` |
| FR-04, FR-05 | scheduler tests with queue-state fixtures and workflow terminal transitions |
| FR-06, FR-08 | queue-error/empty-queue tests validating fallback continuation |
| FR-07, FR-09 | daemon run event payload assertions for additive source telemetry |
| FR-10 | targeted `cargo test -p orchestrator-cli runtime_daemon::daemon_scheduler` and related daemon-run tests |

## Implementation Notes Input (Next Phase)
Primary expected change targets:
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_project_tick.rs`
  - queue-aware selection helper and dispatch-limit wiring.
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler.rs`
  - summary model additions for started-task source telemetry (if needed).
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_run.rs`
  - emit additive source fields in task/workflow start-related daemon events.
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_git_ops.rs`
  - reuse repo-scoped runtime-state path helper if queue persistence helper is
    implemented near daemon runtime state utilities.

Likely supporting surfaces:
- `crates/orchestrator-core/src/services.rs` and/or new queue-state types if a
  typed EM queue API is introduced.
- daemon scheduler tests under
  `crates/orchestrator-cli/src/services/runtime/runtime_daemon/`.

## Deterministic Deliverables for Implementation Phase
- Queue-first scheduler startup behavior with deterministic fallback.
- Queue entry lifecycle handling (`assigned` on start, removal on terminal
  workflow outcome).
- Per-task source telemetry for dispatch origin.
- Regression tests covering precedence, limits, lifecycle, and compatibility.
