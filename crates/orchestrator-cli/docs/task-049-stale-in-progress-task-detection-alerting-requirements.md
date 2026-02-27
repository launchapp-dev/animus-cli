# TASK-049 Requirements: Stale In-Progress Task Detection and Alerting

## Phase
- Workflow phase: `requirements`
- Workflow ID: `eb92de20-d8f2-4695-8b46-dce36c101085`
- Task: `TASK-049`
- Requirement: unlinked in current task metadata

## Objective
Add deterministic stale-task detection for tasks in `in-progress` where
`metadata.updated_at` has not changed within a configurable threshold (default
`24h`), and surface that signal for both operator diagnostics and daemon
telemetry alerting.

## Current Baseline Audit

| Surface | Current location | Current behavior | Gap |
| --- | --- | --- | --- |
| Task statistics | `crates/orchestrator-cli/src/services/runtime/runtime_project_task/task.rs` (`TaskCommand::Stats`) | prints `TaskStatistics` aggregate counts only | no stale in-progress detection/count/list |
| Task statistics model | `crates/orchestrator-core/src/types.rs` (`TaskStatistics`) | includes total/status/priority/type counters | no stale threshold metadata or stale entry summary |
| Daemon queue telemetry | `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_run.rs` | emits queue counters for ready/in-progress/blocked/done and workflow execution counts | no stale-task indicator for alert pipelines |
| Daemon project tick | `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_project_tick.rs` | computes task/workflow counts and emits summary | no non-mutating stale detection output |
| Existing stale reconciliation | `reconcile_stale_in_progress_tasks_for_project` in `daemon_scheduler_project_tick.rs` | can mutate some stale `in-progress` tasks back to `ready` (or terminal mapping) when `--reconcile-stale` is enabled | mutation is not an alert surface and uses a separate 10-minute runtime-reconciliation heuristic |

## Scope
In scope for implementation after this requirements phase:
- Add stale in-progress detection based on task `metadata.updated_at` age.
- Add a threshold option for `ao task stats`:
  - `--stale-threshold-hours <HOURS>` (default `24`, positive integer).
- Add stale summary to `ao task stats` output (JSON and non-JSON).
- Add daemon threshold option for scheduler runs:
  - `ao daemon run --stale-threshold-hours <HOURS>`
  - `ao daemon start --stale-threshold-hours <HOURS>` (forwarded to detached
    `daemon run`).
- Add stale summary fields to daemon `queue` event payload for alert consumers.
- Add deterministic tests for threshold behavior, sorting, and daemon event
  payload shape.

Out of scope for this task:
- Replacing or redesigning `reconcile_stale` status mutation behavior.
- Introducing direct `.ao` file edits outside service APIs.
- Redesigning notification connector routing or delivery policy.
- Adding desktop-wrapper dependencies.

## Constraints
- Stale detection must be read-only:
  - no task status changes,
  - no workflow state changes.
- Existing `task stats` fields must remain available (additive change only).
- Existing daemon `queue` event fields must remain available (additive change
  only).
- Threshold parsing must reject `0` and invalid values with clear input errors.
- Detection and output ordering must be deterministic:
  - stale entries sorted by `updated_at` ascending (oldest first),
  - tie-break by task id ascending.
- Time comparisons must use UTC consistently.
- Detection semantics must remain independent from `--reconcile-stale` mutation
  logic.

## Data Contract

### Stale Detection Rule
A task is stale when all conditions are true:
- `task.status == in-progress`
- `now_utc - task.metadata.updated_at >= threshold_hours`

### `ao task stats` Additive Fields
Add `stale_in_progress` object to stats payload:
- `threshold_hours` (u64)
- `count` (usize)
- `entries` (array) where each entry includes:
  - `task_id`
  - `title`
  - `updated_at` (RFC3339 UTC)
  - `age_hours` (integer floor, non-negative)

### Daemon `queue` Event Additive Fields
Add stale summary fields to emitted queue payload:
- `stale_in_progress_count`
- `stale_in_progress_threshold_hours`
- `stale_in_progress_task_ids` (deterministically ordered stale task ids)

## Functional Requirements

### FR-01: Threshold-Aware Stale Detector
- Implement one shared stale-detector logic path for CLI stats and daemon
  queue telemetry.
- Default threshold is `24h`.
- Threshold is configurable through explicit CLI flags listed in scope.

### FR-02: Task Stats Surface
- `ao task stats` must return stale detection output under
  `stale_in_progress`.
- `--stale-threshold-hours` must override default threshold for that command.
- Existing stats keys must remain unchanged.

### FR-03: Daemon Run/Start Threshold Surface
- `ao daemon run` and detached `ao daemon start` must accept and carry
  `--stale-threshold-hours`.
- Project tick summary must include stale count and stale task ids.

### FR-04: Daemon Queue Alerting Signal
- `queue` daemon event payload must include stale summary fields each tick.
- Non-zero stale count must be machine-detectable without parsing human text.

### FR-05: Read-Only Detection Behavior
- Stale detection itself must not mutate task/workflow state.
- Existing reconcile/mutation paths remain controlled only by existing flags and
  logic.

### FR-06: Deterministic Output
- Stale entries and stale id lists must be deterministically sorted.
- Repeated runs with unchanged task state must produce stable stale ordering.

### FR-07: Backward Compatibility
- Existing `task stats` and daemon queue fields remain present.
- Changes are additive and must not break current consumers expecting existing
  fields.

### FR-08: Regression Coverage
- Add focused tests covering threshold validation, stale classification,
  deterministic ordering, and queue event payload fields.

## Acceptance Criteria
- `AC-01`: `ao task stats` supports `--stale-threshold-hours` and defaults to
  `24`.
- `AC-02`: `ao task stats` output includes `stale_in_progress` with threshold,
  count, and stale entries.
- `AC-03`: stale classification uses `in-progress` + `updated_at` age against
  threshold.
- `AC-04`: stale entries are sorted oldest-first, tie-breaking by task id.
- `AC-05`: `ao daemon run`/`ao daemon start` accept
  `--stale-threshold-hours`.
- `AC-06`: daemon `queue` events include stale summary fields with deterministic
  values.
- `AC-07`: stale detection does not mutate task state.
- `AC-08`: existing task stats and queue fields remain available.
- `AC-09`: targeted tests cover happy path and edge cases for thresholds and
  ordering.

## Testable Acceptance Checklist
- `T-01`: CLI parsing tests for:
  - `task stats --stale-threshold-hours <n>`
  - `daemon run --stale-threshold-hours <n>`
  - `daemon start --stale-threshold-hours <n>`
  - zero-value rejection.
- `T-02`: stale detector unit tests:
  - stale/not-stale boundary at exact threshold,
  - non-`in-progress` exclusion,
  - deterministic ordering.
- `T-03`: `task stats` JSON contract test includes
  `data.stale_in_progress.count`.
- `T-04`: daemon run/scheduler tests verify queue payload includes stale fields
  and forwards threshold.
- `T-05`: regression test verifies stale detection path does not change task
  statuses.

## Verification Matrix

| Requirement area | Verification method |
| --- | --- |
| FR-01, FR-02 | runtime task stats tests + CLI parse tests |
| FR-03, FR-04 | daemon run/project tick tests on queue payload fields |
| FR-05 | unit/integration test asserting no status mutation from detection |
| FR-06 | stale detector ordering tests with controlled timestamps |
| FR-07 | existing contract assertions for pre-existing stats/queue keys |
| FR-08 | targeted `cargo test -p orchestrator-cli` modules listed below |

## Implementation Notes Input (Next Phase)
Primary source targets:
- `crates/orchestrator-cli/src/cli_types/task_types.rs`
- `crates/orchestrator-cli/src/cli_types/daemon_types.rs`
- `crates/orchestrator-cli/src/services/runtime/runtime_project_task/task.rs`
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon.rs`
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler.rs`
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_project_tick.rs`
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_run.rs`

Likely test targets:
- `crates/orchestrator-cli/src/cli_types/mod.rs` tests
- `crates/orchestrator-cli/tests/cli_json_contract.rs`
- daemon runtime tests under
  `crates/orchestrator-cli/src/services/runtime/runtime_daemon/`

## Deterministic Deliverables for Implementation Phase
- Threshold-aware stale in-progress detection with default `24h`.
- `ao task stats` stale summary output and threshold override flag.
- Daemon queue telemetry fields for stale count/threshold/task ids.
- Focused tests proving deterministic detection and additive compatibility.
