# TASK-030 Requirements: Runner Exit Status Propagation and Status Lookup Not-Found Semantics

## Phase
- Workflow phase: `requirements`
- Workflow ID: `e4c951f5-3c9d-4882-8c13-5d2244e80a6e`
- Task: `TASK-030`
- Requirement: `REQ-003`

## Objective
Define the implementation contract that prevents false status reporting in
`agent-runner` by:
- propagating real terminal run outcomes into `Runner::cleanup_agent`
- returning an explicit not-found outcome for unknown run IDs in
  `handle_agent_status`

## Current Baseline (Implemented)

| Surface | Current location | Current behavior | Gap |
| --- | --- | --- | --- |
| Cleanup status persistence | `crates/agent-runner/src/runner/mod.rs` | `cleanup_agent` always stores `AgentStatus::Completed` | failed/errored runs are misreported as completed |
| Supervisor terminal outcome | `crates/agent-runner/src/runner/supervisor.rs` | emits `AgentRunEvent::Finished { exit_code }` on success, `Error` on failure | terminal outcome is not carried into cleanup status |
| Cleanup signaling | `crates/agent-runner/src/runner/mod.rs`, `crates/agent-runner/src/ipc/server.rs` | cleanup channel carries only `RunId` | no path to propagate terminal status metadata |
| Status lookup for unknown run IDs | `crates/agent-runner/src/runner/mod.rs` | returns synthetic `AgentStatusResponse` with `Failed` and fabricated timestamps | lookup miss is indistinguishable from actual execution failure |
| Runner status query protocol | `crates/protocol/src/agent_runner.rs` | only success-shape `AgentStatusResponse` exists | no typed error variant for explicit lookup miss |
| CLI status query parsing | `crates/orchestrator-cli/src/services/runtime/runtime_agent/status.rs` | parses only `AgentStatusResponse`; on runner error, falls back to event-log lookup | typed not-found cannot be surfaced directly |

## Scope
In scope for implementation after this requirements phase:
- propagate terminal run status from supervisor completion path to
  `Runner::cleanup_agent`
- keep deterministic mapping from process outcome to `AgentStatus`
- return typed not-found status query responses for unknown run IDs
- parse typed status-query responses in CLI and preserve not-found semantics
- add targeted protocol, runner, and CLI tests for both defects

Out of scope for this task:
- changing `AgentRunEvent` schema
- changing run-event persistence format/path layout
- changing daemon scheduler behavior or unrelated CLI command output
- manual edits to `.ao/*.json`

## Constraints
- Preserve `stop_agent` termination semantics:
  - `AgentStatus::Terminated` set by `stop_agent` must not be overwritten by
    asynchronous cleanup writeback.
- Keep protocol changes additive and backward-compatible:
  - existing `AgentStatusResponse` shape remains valid.
- Keep behavior deterministic:
  - `exit_code == 0` maps to `AgentStatus::Completed`
  - `exit_code != 0` maps to `AgentStatus::Failed`
  - supervisor/process errors without exit code map to `AgentStatus::Failed`
- Keep CLI fallback behavior constrained:
  - event-log fallback is allowed for transport/no-response failures
  - event-log fallback is not allowed to mask typed runner `not_found` response

## Functional Requirements

### FR-01: Terminal Status Propagation Through Cleanup
- Runner cleanup signaling must carry terminal status along with run ID.
- `cleanup_agent` must persist the terminal status it receives instead of a
  hard-coded `Completed`.

### FR-02: Deterministic Outcome Mapping
- Supervisor path must provide a deterministic terminal status value from process
  execution result:
  - successful exit code `0` => `Completed`
  - successful non-zero exit code => `Failed`
  - process execution error => `Failed`
  - workspace validation failure => `Failed`

### FR-03: Unknown Run IDs Are Not Execution Failures
- Unknown run ID lookup in `handle_agent_status` must return a typed not-found
  response, not a synthetic `Failed` status object.
- No fabricated timestamps may be emitted for lookup misses.

### FR-04: Typed Status Query Response Contract
- Protocol must support status-query success and error outcomes explicitly.
- Error payload must include a machine-readable code that supports
  not-found-specific handling.

### FR-05: CLI Not-Found Semantics
- CLI status query path must parse typed success/error status responses.
- Typed not-found response from runner must surface as a not-found-classified
  CLI error (maps to exit code `3` through existing classifier).

### FR-06: Regression Coverage
- Add tests covering terminal status propagation and unknown-run not-found
  status behavior.
- Add protocol serialization tests for status-query success and error variants.

## Acceptance Criteria
- `AC-01`: Cleanup writes `Completed` only when terminal outcome is successful
  exit code `0`; non-zero/errored outcomes persist `Failed`.
- `AC-02`: `stop_agent` terminal status (`Terminated`) remains authoritative and
  is not overwritten by later cleanup events.
- `AC-03`: Unknown run ID status queries return typed `not_found` response, not
  synthetic `Failed` with generated timestamps.
- `AC-04`: Protocol supports and roundtrips both status success and status error
  query responses.
- `AC-05`: CLI parses the new status-query response shapes and returns a
  not-found error when runner reports typed not-found.
- `AC-06`: CLI event-log fallback remains available for runner transport/no
  response failure paths.
- `AC-07`: Changes remain scoped to runner/protocol/CLI status surfaces without
  unrelated behavior regressions.

## Testable Acceptance Checklist
- `T-01`: `agent-runner` tests for cleanup status propagation (`0`, non-zero,
  and supervisor error paths).
- `T-02`: `agent-runner` tests for unknown-run typed not-found status query
  behavior.
- `T-03`: `protocol` tests for status-query success/error serialization
  roundtrips.
- `T-04`: `orchestrator-cli` tests for typed not-found parsing and fallback
  boundary behavior.

## Acceptance Verification Matrix

| Requirement area | Verification method |
| --- | --- |
| FR-01, FR-02 | `agent-runner` unit tests in runner lifecycle/status paths |
| FR-03 | `agent-runner` status lookup tests for unknown run IDs |
| FR-04 | `protocol` serialization tests (`agent_runner` test module) |
| FR-05 | `orchestrator-cli` runtime-agent status parsing tests |
| FR-06 | targeted crate test runs across `agent-runner`, `protocol`, `orchestrator-cli` |

## Implementation Notes (Input to Next Phase)
Primary code surfaces:
- `crates/protocol/src/agent_runner.rs`
- `crates/protocol/tests/agent_runner.rs`
- `crates/agent-runner/src/runner/mod.rs`
- `crates/agent-runner/src/runner/supervisor.rs`
- `crates/agent-runner/src/ipc/server.rs`
- `crates/agent-runner/src/ipc/handlers/status.rs`
- `crates/orchestrator-cli/src/services/runtime/runtime_agent/status.rs`

Reference research artifact:
- `crates/orchestrator-cli/docs/task-030-runner-status-research-notes.md`

## Deterministic Deliverables for Implementation Phase
- Additive status-query protocol contract with typed not-found error path.
- Runner cleanup path that persists true terminal status.
- CLI status parsing that distinguishes typed not-found from transport failure.
- Focused regression tests validating terminal status and lookup semantics.
