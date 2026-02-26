# TASK-003 Requirements: Runner Lifecycle Regression Coverage

## Phase
- Workflow phase: `requirements`
- Workflow ID: `2e268879-14ac-4cac-89c7-862f813b1a45`
- Task: `TASK-003`

## Objective
Define deterministic regression coverage for runner lifecycle behavior used by
AO daemon workflows so startup, health fallback, stale artifact cleanup, and
shutdown remain stable across refactors.

## Existing Baseline Audit

| Coverage area | Current location | Current state | Gap |
| --- | --- | --- | --- |
| Startup + readiness loop | `crates/orchestrator-core/src/services/runner_helpers.rs` (`ensure_agent_runner_running`) | production logic exists for stale cleanup, readiness checks, spawn, and warm-up retries | no regression tests for lifecycle branches |
| Startup fallback retry | `crates/orchestrator-core/src/services/daemon_impl.rs` (`FileServiceHub::start`) | first startup failure triggers one stop+retry attempt | no test proving fallback behavior and retry error context |
| Health-check degradation while running | `crates/orchestrator-core/src/services/daemon_impl.rs` (`FileServiceHub::status`) | daemon transitions to `Crashed` and logs error when runner is gone | no regression test for state transition + diagnostic log |
| Stale lock/socket cleanup | `crates/orchestrator-core/src/services/runner_helpers.rs` (`clear_stale_runner_artifacts`) | stale lock and unix socket cleanup paths implemented | no tests for dead-PID lock and stale socket cleanup branches |
| Shutdown lifecycle | `crates/orchestrator-core/src/services/runner_helpers.rs` (`stop_agent_runner_process`) | graceful terminate with forced kill fallback and artifact removal | no tests for live/dead/missing-lock shutdown cases |
| Current tests | `crates/orchestrator-core/src/services/runner_helpers.rs` | only protocol/build compatibility unit tests | lifecycle behavior is effectively unguarded |

## Scope
In scope for implementation after this requirements phase:
- Add regression tests for runner lifecycle behavior in
  `crates/orchestrator-core`.
- Cover startup behaviors:
  - existing compatible runner is reused
  - incompatible/failed startup path executes one fallback stop+retry
  - retry failure returns contextual error including first failure cause
- Cover health degradation behavior:
  - daemon running/paused state transitions to `Crashed` when runner health
    and process liveness are both lost
  - diagnostic log entry for health-check failure is recorded
- Cover stale artifact cleanup behavior:
  - stale lock file removed when PID is not alive
  - stale unix socket removed when no active runner is reachable
- Cover shutdown behavior:
  - missing/dead lock paths are handled without false success
  - live runner termination removes lock/socket artifacts
- Add repeated lifecycle regression:
  - minimum 20 start/stop cycles in deterministic test flow with no orphaned
    lock/socket artifacts after each cycle

Out of scope for this task:
- Protocol/schema changes for runner status payloads.
- AO CLI envelope/exit-code behavior changes.
- Wrapper/MCP endpoint orchestration changes.
- Manual edits to `.ao/*.json`.

## Constraints
- Tests must be deterministic and isolated:
  - use temp project/config roots
  - avoid dependence on any globally running `agent-runner`
- Keep process safety:
  - terminate only fixture/test-owned processes
  - never target arbitrary host PIDs
- Keep runtime bounded:
  - use short readiness/wait windows in test paths
  - avoid long wall-clock sleeps
- Keep platform behavior explicit:
  - unix socket cleanup assertions behind `#[cfg(unix)]`
  - non-unix coverage still validates lock/shutdown invariants
- Keep implementation repository-safe:
  - no destructive git operations
  - no direct `.ao` file mutation

## Regression Scenario Matrix

| Case ID | Scenario | Entry point | Required assertions |
| --- | --- | --- | --- |
| `RL-01` | Startup reuses compatible ready runner | `ensure_agent_runner_running` | no new spawn; returns existing PID |
| `RL-02` | Startup fallback executes stop+retry after initial failure | `FileServiceHub::start` | one stop call between attempts; daemon starts successfully |
| `RL-03` | Retry failure preserves first-failure context | `FileServiceHub::start` | error includes retry context and first failure summary |
| `RL-04` | Daemon status marks runner-loss crash | `FileServiceHub::status` | status becomes `Crashed`; error log emitted |
| `RL-05` | Stale lock file cleanup for dead PID | `clear_stale_runner_artifacts` | lock removed when PID is dead |
| `RL-06` | Stale unix socket cleanup when unreachable | `clear_stale_runner_artifacts` | socket removed (unix only) |
| `RL-07` | Shutdown with missing/dead lock is non-destructive | `stop_agent_runner_process` | returns `false`; stale artifacts cleaned |
| `RL-08` | Shutdown with live PID terminates and cleans up | `stop_agent_runner_process` | returns `true`; lock/socket removed |
| `RL-09` | 20-cycle lifecycle stability | daemon start/stop workflow path | no orphaned lock/socket artifacts across cycles |

## Acceptance Criteria
- `AC-01`: Regression tests cover startup reuse and startup fallback retry
  branches (`RL-01`, `RL-02`, `RL-03`).
- `AC-02`: Regression tests cover daemon crash transition and diagnostic log
  behavior after runner health loss (`RL-04`).
- `AC-03`: Regression tests cover stale lock cleanup for dead PID (`RL-05`).
- `AC-04`: Regression tests cover stale unix socket cleanup on unix (`RL-06`).
- `AC-05`: Regression tests cover shutdown behavior for missing/dead/live lock
  states (`RL-07`, `RL-08`).
- `AC-06`: A deterministic 20-cycle lifecycle test proves no orphaned lock or
  socket artifacts remain (`RL-09`).
- `AC-07`: New tests run in isolated temp roots and do not require manual daemon
  setup.
- `AC-08`: Existing `orchestrator-core` test suites remain green.

## Verification Matrix

| Requirement | Verification method |
| --- | --- |
| `AC-01`, `AC-05` | lifecycle-focused tests around `runner_helpers` startup/shutdown branches |
| `AC-02` | daemon status regression test asserting `Crashed` transition + log emission |
| `AC-03`, `AC-04` | stale artifact tests with temp lock/socket fixtures |
| `AC-06` | bounded 20-cycle start/stop regression test with artifact assertions |
| `AC-07` | test harness env guards + temp dirs per test |
| `AC-08` | `cargo test -p orchestrator-core` |

## Deterministic Deliverables for Implementation Phase
- Add runner lifecycle regression tests in `crates/orchestrator-core` for
  startup, fallback, stale cleanup, and shutdown.
- Introduce minimal test seams only where needed to make lifecycle behavior
  deterministic without relying on external/global runner processes.
- Keep public runtime behavior unchanged while increasing regression coverage.
