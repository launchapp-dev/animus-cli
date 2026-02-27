# TASK-045 Requirements: Unified `ao status` Dashboard Command

## Phase
- Workflow phase: `requirements`
- Workflow ID: `14d8d75d-e8e2-483e-9ced-9a1f0e9fe029`
- Task: `TASK-045`
- Requirement: unlinked in current task metadata

## Objective
Define a deterministic, read-only `ao status` command that provides one unified
project snapshot without requiring operators to stitch together multiple
commands.

## Current Baseline (Implemented)

| Surface | Current location | Current behavior | Gap |
| --- | --- | --- | --- |
| Daemon health | `ao daemon health` (`runtime_daemon.rs`) | returns status, runner connectivity, runner pid, active agent count | no unified snapshot with tasks/workflows/completions/failures/CI |
| Active agents | `ao daemon agents` (`runtime_daemon.rs`) | returns count only | no task/workflow attribution |
| Task summary | `ao task stats` (`runtime_project_task/task.rs`) | returns totals and aggregate counters | no dashboard context, no recent completions list |
| Workflow status/failures | `ao workflow list` (`ops_workflow.rs`) | returns raw workflow list | no focused "last failed phases" view |
| Execution history | `ao history recent` (`ops_history.rs`) | returns mixed execution records | not task-completion-focused dashboard slice |
| CI status | none | no first-class command for latest GitHub Actions run | operator must call `gh` manually |

## Scope
In scope for implementation after this requirements phase:
- Add new top-level command: `ao status`.
- Provide a single snapshot that includes:
  - daemon health (`running/stopped`, `runner_connected`, `runner_pid`),
  - active agents (count and task/workflow context),
  - task summary (`total`, `done`, `in_progress`, `ready`, `blocked`),
  - recent completions (last 5 completed tasks with timestamps),
  - recent failures (last 3 failed workflow phases),
  - CI status (latest workflow run if `gh` is available).
- Support `--json` with deterministic machine-readable fields for MCP
  consumption.
- Add tests and docs for command behavior and degraded-mode paths.

Out of scope for this task:
- Redesigning daemon event schema or workflow state-machine semantics.
- Adding a new external CI provider integration beyond GitHub CLI (`gh`).
- Mutating `.ao` state to compute status.
- Adding desktop-specific dependencies or non-Rust wrappers.

## Constraints
- Command must be read-only and deterministic.
- Preserve global CLI envelope contract for `--json` (`ao.cli.v1`).
- Partial data unavailability (daemon down, missing `gh`, parse failures) must
  not fail the entire command; report section-level status instead.
- Sorting and truncation must be stable:
  - recent completions: descending by completion timestamp, then task id;
  - recent failures: descending by failure timestamp, then workflow id.
- Task "done" count must reflect `status == done` only (not cancelled).
- Keep project-root scoping explicit and deterministic via existing root
  resolution flow.

## Data Contract

### Daemon Health Slice
- Source: `hub.daemon().health()`.
- Required fields:
  - `status` (`running|paused|stopped|starting|stopping|crashed`),
  - derived `running` boolean (true for `running`/`paused`),
  - `runner_connected`,
  - `runner_pid`.

### Active Agents Slice
- Primary count source: daemon health `active_agents`.
- Task/workflow attribution source: running workflows from
  `hub.workflows().list()` joined with task metadata from `hub.tasks().list()`.
- Each entry must include at least: `task_id`, `task_title`, `workflow_id`,
  `phase_id`.
- If count exceeds attributable running workflows, include deterministic
  placeholder entries indicating unknown assignment rather than dropping count
  fidelity.

### Task Summary Slice
- Source: `hub.tasks().statistics()` and/or task list as needed.
- Required counters:
  - `total`,
  - `done` (`by_status["done"]`, default `0`),
  - `in_progress`,
  - `ready` (`by_status["ready"]`, default `0`),
  - `blocked` (blocked + on-hold behavior from existing status semantics).

### Recent Completions Slice
- Source: task list (`hub.tasks().list()`).
- Include tasks with `status == done` and non-empty `metadata.completed_at`.
- Return last 5, including: `task_id`, `title`, `completed_at`.

### Recent Failures Slice
- Source: workflow list (`hub.workflows().list()`).
- Include workflows with `status == failed`.
- Derive failed phase from latest phase marked `failed`; fallback to
  `current_phase` then `"unknown"`.
- Return last 3, including: `workflow_id`, `task_id`, `phase_id`, `failed_at`,
  `failure_reason`.

### CI Status Slice
- If `gh` is unavailable: emit `available: false` with reason.
- If `gh` is available: query most recent run in current repo and return run
  summary fields.
- CI lookup errors must be captured as CI-slice status, not command-fatal
  errors.

## Functional Requirements

### FR-01: Command Surface
- Add a new top-level CLI command: `ao status`.
- Command must respect global flags (`--project-root`, `--json`).

### FR-02: Unified Snapshot Assembly
- `ao status` must compute a single in-memory snapshot composed of daemon,
  tasks, workflows, and optional CI slices.

### FR-03: Human Output
- Without `--json`, command must render a readable sectioned dashboard view in
  stable order:
  1. daemon,
  2. active agents,
  3. task summary,
  4. recent completions,
  5. recent failures,
  6. CI status.

### FR-04: JSON Output for MCP
- With `--json`, command must emit deterministic structured data under the
  existing `ao.cli.v1` success envelope.
- Data payload must include a command schema marker (for example
  `ao.status.v1`) and all required slices.

### FR-05: Active Agent Attribution
- Active-agent count and listed assignments must be present in every response.
- Attribution must include task/workflow identifiers when derivable.

### FR-06: Recent Completion and Failure Windows
- Completion window fixed to 5 records.
- Failure window fixed to 3 records.
- Both windows must use stable sorting and deterministic tie-breakers.

### FR-07: CI Optionality and Degraded Mode
- CI section must exist even when `gh` is missing or lookup fails.
- Missing `gh` or CI query failures must not change process exit code to error.

### FR-08: Backward Compatibility
- Existing commands (`daemon health`, `task stats`, `workflow list`, etc.) must
  remain unchanged.

### FR-09: Regression Coverage
- Add tests for JSON shape, aggregation semantics, ordering/limits, and
  degraded-mode behavior.

## Acceptance Criteria
- `AC-01`: `ao status` exists and executes for the resolved project root.
- `AC-02`: Output includes daemon status, runner connectivity, and runner pid.
- `AC-03`: Output includes active-agent count plus task/workflow attribution
  entries.
- `AC-04`: Output includes task summary counters:
  `total|done|in_progress|ready|blocked`.
- `AC-05`: Output includes at most 5 recent completed tasks, sorted by
  completion time descending.
- `AC-06`: Output includes at most 3 recent failed workflow phases, sorted by
  failure time descending.
- `AC-07`: CI status is included and degrades gracefully when `gh` is missing
  or query fails.
- `AC-08`: `--json` output is machine-consumable and deterministic under
  `ao.cli.v1`.
- `AC-09`: Existing command behavior outside `ao status` is unchanged.
- `AC-10`: Targeted tests cover happy path and degraded paths.

## Testable Acceptance Checklist
- `T-01`: CLI parse/dispatch test for new `status` top-level command.
- `T-02`: Unit tests for aggregation logic (counts, joins, sorting, limits).
- `T-03`: Unit tests for recent completions filtering (`done` + timestamp).
- `T-04`: Unit tests for failed-phase extraction from workflow data.
- `T-05`: CI lookup tests for:
  - `gh` unavailable,
  - successful latest-run parse,
  - command error/invalid payload fallback.
- `T-06`: Output tests validating non-JSON section ordering and JSON payload
  schema marker.

## Acceptance Verification Matrix

| Requirement area | Verification method |
| --- | --- |
| FR-01, FR-02 | `orchestrator-cli` command dispatch + status assembly tests |
| FR-03 | non-JSON rendering tests (section ordering, core labels) |
| FR-04 | JSON snapshot shape test under `ao.cli.v1` envelope |
| FR-05 | active-agent attribution tests from running workflow fixtures |
| FR-06 | deterministic sort/limit tests for completions and failures |
| FR-07 | CI degraded-mode tests with mocked `gh` availability/error |
| FR-08 | targeted regression tests for neighboring command surfaces |
| FR-09 | focused crate tests for new status module |

## Implementation Notes (Input to Next Phase)
Primary source targets:
- `crates/orchestrator-cli/src/cli_types.rs`
- `crates/orchestrator-cli/src/main.rs`
- `crates/orchestrator-cli/src/services/operations.rs`
- new status handler module under
  `crates/orchestrator-cli/src/services/operations/`
- existing data-provider modules:
  - `runtime_daemon.rs`,
  - `runtime_project_task/task.rs`,
  - `ops_workflow.rs`.

Supporting references:
- `crates/orchestrator-core/src/types.rs` (`DaemonHealth`, `TaskStatistics`,
  workflow/task structures).
- `crates/orchestrator-core/src/services.rs` service API surface.

## Deterministic Deliverables for Implementation Phase
- New `ao status` command with unified, read-only dashboard snapshot.
- Stable JSON payload suitable for MCP callers via `--json`.
- Graceful CI integration behavior when `gh` is unavailable or fails.
- Focused tests proving ordering, limits, and degraded-mode correctness.
