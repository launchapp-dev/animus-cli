# TASK-107 Requirements: Detached Daemon Startup Survival Validation

## Phase
- Workflow phase: `requirements`
- Workflow ID: `82e79c48-3002-4cf0-a798-2e41d5ea2e8a`
- Task: `TASK-107`
- Requirement: unlinked in current task metadata

## Objective
Prevent false-positive success from `ao daemon start --autonomous` by verifying
that the spawned `daemon run` subprocess survives a startup grace window. If the
subprocess exits immediately, surface a deterministic failure that includes
startup log context. Ensure MCP `ao.daemon.start` returns the same failure
signal.

## Current Baseline Audit

| Surface | Current location | Current behavior | Gap |
| --- | --- | --- | --- |
| Detached spawn path | `crates/orchestrator-cli/src/services/runtime/runtime_daemon.rs` (`spawn_autonomous_daemon_run`) | spawns child and returns `pid` immediately; stdio redirected to `/dev/null` | no startup survival validation, no startup diagnostics |
| Autonomous start handler | `runtime_daemon.rs` (`handle_daemon`, `DaemonCommand::Start`) | stores daemon pid in registry and returns `"daemon started"` immediately after spawn | false success possible when child exits moments later |
| MCP daemon start tool | `crates/orchestrator-cli/src/services/operations/ops_mcp.rs` (`ao.daemon.start`) | forwards CLI daemon-start call and mirrors CLI envelope | inherits false success because CLI reports success too early |
| Regression coverage | `crates/orchestrator-cli/tests/cli_e2e.rs` | validates idempotent autonomous start/stop path | no test for immediate child crash after spawn |

## Scope
In scope for implementation after this requirements phase:
- Add a post-spawn startup probe for autonomous daemon starts.
- Probe window must be between 3 and 5 seconds (inclusive) and deterministic.
- If subprocess exits during probe window:
  - treat startup as failure,
  - read and surface startup log tail from the task-scoped daemon-start log,
  - avoid persisting a live daemon pid in registry state.
- Keep MCP `ao.daemon.start` behavior aligned with CLI outcome (success only
  when probe passes, structured error when probe fails).
- Add deterministic tests for startup-pass and startup-fail behavior.

Out of scope for this task:
- Redesigning foreground `ao daemon run` lifecycle semantics.
- Redesigning daemon event schema or notification routing.
- Broad MCP response contract changes unrelated to daemon start.
- Manual edits to `.ao/*.json` state files.

## Constraints
- Startup validation must be additive and deterministic.
- Detached start success must only be returned after probe completion.
- Probe failure must clear stale registry pid state for the project.
- Log-read behavior must be bounded (tail only) and resilient when log file is
  missing/unreadable.
- Existing success payload fields for autonomous start (`message`, `autonomous`,
  `daemon_pid`) must remain available when startup succeeds.
- Existing non-autonomous daemon start behavior must remain unchanged.
- Cross-platform process-liveness checks must continue to use existing
  platform-specific helpers.

## Functional Requirements

### FR-01: Startup Survival Probe
- After spawning autonomous `daemon run`, the start handler must wait within a
  deterministic 3-5 second probe window and verify subprocess liveness.
- Passing condition: process is alive at end of probe window.

### FR-02: Immediate-Crash Failure Path
- If subprocess exits before probe completion, `daemon start --autonomous` must
  return an error (not success).
- Registry state must not retain the dead daemon pid.

### FR-03: Startup Log Diagnostics
- On FR-02 failure, startup error text must include:
  - the startup log file path used for detached daemon run logging,
  - bounded tail content (or an explicit fallback reason if unavailable).
- Diagnostic reporting must not panic even when the log file does not exist.

### FR-04: MCP Parity for `ao.daemon.start`
- MCP `ao.daemon.start` must reflect the same startup probe semantics as CLI.
- On FR-02 failure, MCP tool must return structured error content rather than a
  successful `result` payload.

### FR-05: Backward Compatibility
- Existing autonomous idempotency behavior (already-running daemon path) remains
  intact.
- Existing non-autonomous `daemon start` path remains intact.

### FR-06: Regression Coverage
- Add targeted tests for:
  - autonomous start success when child stays alive through probe,
  - autonomous start failure when child exits early,
  - MCP `ao.daemon.start` returning error on early-exit startup failures.

## Acceptance Criteria
- `AC-01`: `ao daemon start --autonomous` waits for startup probe completion
  before returning success.
- `AC-02`: probe duration is deterministic and within 3-5 seconds.
- `AC-03`: early child exit during probe returns CLI error (no success message).
- `AC-04`: failure message includes startup log path and bounded log-tail
  diagnostics (or deterministic fallback text).
- `AC-05`: registry daemon pid is not left pointing to a dead process after
  early-exit startup failure.
- `AC-06`: successful startup still returns `daemon_pid` and existing fields.
- `AC-07`: MCP `ao.daemon.start` returns structured error for early-exit
  startup failure.
- `AC-08`: existing already-running autonomous start behavior remains stable.
- `AC-09`: targeted tests cover success and failure startup probe paths.

## Testable Acceptance Checklist
- `T-01`: unit/integration test for startup probe pass (live subprocess survives
  probe window).
- `T-02`: unit/integration test for startup probe fail (subprocess exits before
  probe window completes).
- `T-03`: assertion that failed startup path does not persist dead daemon pid in
  registry.
- `T-04`: assertion that failure diagnostics include startup log context.
- `T-05`: MCP-facing test ensuring `ao.daemon.start` produces
  `CallToolResult::structured_error` when startup probe fails.

## Verification Matrix

| Requirement area | Verification method |
| --- | --- |
| FR-01, FR-02 | runtime daemon start handler tests + CLI e2e startup-failure path |
| FR-03 | startup-log-tail helper tests and failure-path assertion |
| FR-04 | `ops_mcp` tests for daemon start error propagation |
| FR-05 | existing autonomous idempotency and non-autonomous start tests |
| FR-06 | targeted `cargo test -p orchestrator-cli` for touched modules |

## Implementation Notes Input (Next Phase)
Primary source targets:
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon.rs`
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_registry.rs`
- `crates/orchestrator-cli/src/services/operations/ops_mcp.rs`
- `crates/orchestrator-cli/tests/cli_e2e.rs`

Likely supporting targets:
- helper utilities in `runtime_daemon.rs` for probe timing and log-tail reading.

## Deterministic Deliverables for Implementation Phase
- Autonomous daemon start returns success only after probe passes.
- Early startup crash returns actionable error with startup-log context.
- Registry pid state remains consistent on startup failure.
- MCP `ao.daemon.start` mirrors CLI success/failure semantics.
- Focused regression tests covering probe pass/fail and MCP propagation.
