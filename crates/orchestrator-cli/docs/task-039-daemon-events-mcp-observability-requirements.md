# TASK-039 Requirements: MCP Daemon Event Polling and Scheduler Observability

## Phase
- Workflow phase: `requirements`
- Workflow ID: `03101af3-8067-4035-b516-ff2444151c22`
- Task: `TASK-039`
- Requirement: unlinked in current task metadata

## Objective
Define a deterministic contract that fixes `ao.daemon.events` over MCP and
exposes scheduler outcomes as queryable daemon events without breaking existing
CLI streaming behavior.

## Current Baseline (Implemented)

| Surface | Current location | Current behavior | Gap |
| --- | --- | --- | --- |
| MCP daemon events tool | `crates/orchestrator-cli/src/services/operations/ops_mcp.rs` (`ao_daemon_events`) | invokes `ao daemon events --follow false --limit <n>` | tool execution path expects a single JSON payload |
| MCP CLI output parsing | `ops_mcp.rs` (`parse_json`) | parses `stdout` as one JSON document | multi-line JSONL output cannot be parsed; MCP result becomes `null` |
| CLI daemon events command | `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_events.rs` | with `--json`, prints one JSON event per line | optimized for stream/tail, not request/response envelopes |
| Scheduler summary emission | `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_run.rs` | emits `queue`/`workflow` events including started/executed/failed counts | not currently consumed through a polling-safe MCP payload |
| Scheduler phase/task events | `daemon_run.rs`, `daemon_scheduler_project_tick.rs`, `daemon_scheduler_phase_exec.rs` | emits `task-state-change` and `workflow-phase-*` events | no MCP contract guarantees these are returned in structured polling responses |
| Tick error emission | `daemon_run.rs` | emits `log` event (`level=error`) when project tick returns error | no MCP contract guaranteeing error events are queryable in same polling interface |

## Scope
In scope for implementation after this requirements phase:
- Make `ao.daemon.events` MCP behavior polling-safe and deterministic for
  request/response usage.
- Return recent daemon events as structured JSON data (`events` array), not raw
  stream text parsing.
- Ensure scheduler outcome signals are queryable through the polling response:
  - tasks started,
  - phases executed/failed,
  - scheduler errors.
- Add targeted tests for MCP daemon-event polling and scheduler event coverage.
- Keep documentation aligned with final MCP behavior.

Out of scope for this task:
- Replacing daemon event schema `ao.daemon.event.v1`.
- Removing or redesigning CLI follow/stream semantics for `ao daemon events`.
- Reworking notification connector architecture.
- Manual edits to `.ao/*.json`.

## Constraints
- Preserve compatibility for existing daemon event records (`schema`, `id`,
  `seq`, `timestamp`, `event_type`, `project_root`, `data`).
- Keep changes additive for operators already using `ao daemon events` in
  terminal streaming mode.
- Maintain deterministic ordering in polling results (sequence order).
- Keep polling bounds safe and explicit (`limit` must remain bounded and
  validated).
- Keep project scoping deterministic: filtering by project root must not return
  unrelated project events.

## Functional Requirements

### FR-01: Polling-Safe MCP Daemon Events Response
- MCP `ao.daemon.events` must return structured JSON containing recent daemon
  events and metadata suitable for polling.
- Response must not depend on follow/stream semantics.
- For non-empty event logs, `result` must be non-null and include parsed events.

### FR-02: Deterministic Event Retrieval Contract
- Polling response must support bounded retrieval of the most recent events
  (`limit`).
- Events must be returned in deterministic sequence order.
- Invalid/empty lines in daemon event log must not crash polling; malformed
  lines are skipped deterministically.

### FR-03: Project-Scoped Queryability
- Polling contract must support project-root scoping consistent with daemon
  event consumers in this repository.
- When project root is provided, events for other project roots must be
  excluded.

### FR-04: Scheduler Outcome Observability Coverage
- Queryable daemon events must expose scheduler tick outcomes for:
  - tasks started (`queue.started_ready_workflows` and/or task transition events),
  - phase outcomes (`queue.executed_workflow_phases`,
    `queue.failed_workflow_phases`, and phase execution events),
  - errors (`log` events with error level and relevant failure events).
- Required summary fields in emitted `queue` event payload remain present and
  documented:
  - `started_ready_workflows`
  - `executed_workflow_phases`
  - `failed_workflow_phases`

### FR-05: Backward-Compatible CLI Behavior
- `ao daemon events --follow true/false` CLI behavior remains valid for terminal
  use.
- MCP polling implementation must not require breaking changes to CLI flags.

### FR-06: Regression Test Coverage
- Add tests that prove MCP daemon event polling returns structured events instead
  of `null`.
- Add/extend daemon runtime tests that validate scheduler summary/error event
  fields required by FR-04.

## Acceptance Criteria
- `AC-01`: MCP `ao.daemon.events` returns structured `events` data (not `null`)
  when daemon events exist.
- `AC-02`: Polling response is deterministic and bounded by requested limit.
- `AC-03`: Polling response supports project-root scoping and excludes unrelated
  project events.
- `AC-04`: Scheduler outcomes for started tasks, phase execution/failure counts,
  and tick errors are queryable through returned daemon events.
- `AC-05`: Existing CLI daemon event follow/tail behavior remains available.
- `AC-06`: Targeted tests cover MCP polling behavior and scheduler observability
  contract.

## Testable Acceptance Checklist
- `T-01`: `ops_mcp` unit tests for daemon events response shaping and JSON parsing
  of multi-line event logs.
- `T-02`: daemon events reader tests for bounded tail retrieval and ordering.
- `T-03`: daemon run tests verifying `queue` payload includes required scheduler
  summary fields.
- `T-04`: daemon run/error path tests verifying queryable error event emission.

## Acceptance Verification Matrix

| Requirement area | Verification method |
| --- | --- |
| FR-01, FR-02 | `orchestrator-cli` MCP tool tests for structured polling output |
| FR-03 | project-root scoped polling tests using mixed project event fixtures |
| FR-04 | daemon runtime tests inspecting emitted `queue`, `task-state-change`, phase, and `log` events |
| FR-05 | CLI daemon events regression tests for non-MCP behavior |
| FR-06 | targeted crate test run covering MCP + runtime daemon modules |

## Implementation Notes (Input to Next Phase)
Primary source targets:
- `crates/orchestrator-cli/src/services/operations/ops_mcp.rs`
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_events.rs`
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_run.rs`
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_project_tick.rs`
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_phase_exec.rs`

Suggested supporting reference:
- `crates/orchestrator-web-api/src/services/web_api_service.rs` event log reading
  approach (`read_events_for_project`) for deterministic filtering/parsing
  behavior.

## Deterministic Deliverables for Implementation Phase
- MCP daemon events polling response with structured, non-null `events` output.
- Deterministic daemon event tail reader with bounded limit and project scoping.
- Scheduler outcome fields/events verifiably queryable through MCP polling.
- Focused regression tests for MCP daemon events and scheduler observability.
