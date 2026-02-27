# TASK-039 Implementation Notes: MCP Daemon Event Polling and Scheduler Observability

## Phase Context
- Workflow phase: `requirements`
- Workflow ID: `03101af3-8067-4035-b516-ff2444151c22`
- Task: `TASK-039`

## Purpose
Translate TASK-039 into a concrete implementation slice that:
- fixes `ao.daemon.events` MCP results returning `null`, and
- guarantees scheduler outcomes are available as queryable daemon events through
  a polling-safe interface.

## Non-Negotiable Constraints
- Keep daemon event schema `ao.daemon.event.v1` unchanged.
- Keep terminal CLI `ao daemon events` follow/tail behavior intact.
- Keep changes scoped to `orchestrator-cli` daemon/MCP event surfaces.
- Preserve deterministic ordering and bounded result size.
- Do not manually edit `.ao/*.json`.

## Proposed Change Surface

### 1) Introduce a Polling-Oriented Daemon Event Reader
- Target: `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_events.rs`
- Add reusable helpers that:
  - read daemon event JSONL entries safely,
  - parse into `DaemonEventRecord`,
  - optionally filter by project root,
  - return deterministic tail slices by `limit`.
- Keep streaming-oriented CLI loop (`handle_daemon_events_impl`) behavior
  available for terminal use.

### 2) Fix MCP `ao.daemon.events` Output Shaping
- Target: `crates/orchestrator-cli/src/services/operations/ops_mcp.rs`
- Replace raw command-output parsing dependency for daemon events with structured
  polling data from daemon event reader helpers.
- Return MCP structured payload with explicit shape:
  - `schema` (polling response schema id),
  - `events_path`,
  - `count`,
  - `events` (array of daemon event records).
- Keep existing `DaemonEventsInput` contract (`limit`, `project_root`) unless
  additive fields are required.

### 3) Preserve Scheduler Observability Contract
- Targets:
  - `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_run.rs`
  - `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_project_tick.rs`
  - `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_phase_exec.rs`
- Verify and lock required scheduler outcome surfaces:
  - `queue.started_ready_workflows`,
  - `queue.executed_workflow_phases`,
  - `queue.failed_workflow_phases`,
  - `task-state-change` transitions,
  - phase execution events,
  - `log` errors for tick failures.
- If gaps are found while implementing, add minimal additive event emission to
  satisfy requirements without schema churn.

### 4) Tests
- `ops_mcp.rs`:
  - add tests for daemon-event polling result shape and non-null data behavior.
  - add tests validating bounded limit and project scoping behavior.
- `runtime_daemon/daemon_events.rs`:
  - add unit tests for deterministic parsing/tailing/filtering.
- `runtime_daemon/daemon_run.rs`:
  - extend/add tests verifying required `queue` fields and queryable error
    event coverage.

## Suggested Implementation Sequence
1. Add daemon event polling reader helper(s) in `daemon_events.rs` with tests.
2. Wire `ao.daemon.events` MCP tool to structured polling output.
3. Verify scheduler summary/error event coverage and patch minimal gaps.
4. Add/adjust MCP + daemon runtime tests.
5. Run targeted tests and fix regressions introduced by TASK-039 changes.

## Validation Targets
- `cargo test -p orchestrator-cli services::operations::ops_mcp`
- `cargo test -p orchestrator-cli runtime_daemon::daemon_events`
- `cargo test -p orchestrator-cli runtime_daemon::daemon_run`
- Optional broader guard after targeted pass:
  - `cargo test -p orchestrator-cli`

## Risks and Mitigations
- Risk: duplicated daemon event parsing logic diverges across crates.
  - Mitigation: keep parsing behavior centralized in daemon runtime module and
    reuse from MCP path.
- Risk: unbounded polling payloads increase response size.
  - Mitigation: enforce validated `limit` and deterministic tail slicing.
- Risk: accidental regression to CLI follow mode behavior.
  - Mitigation: avoid changing follow loop semantics; add regression coverage.
- Risk: scheduler error semantics are ambiguous across event types.
  - Mitigation: document required event types/fields and assert them in tests.

## Deliverables for Next Phase
- Non-null, structured MCP daemon events polling response.
- Deterministic, test-covered daemon event tail/filter logic.
- Confirmed queryability for scheduler start/phase/error outcomes via daemon
  events.
