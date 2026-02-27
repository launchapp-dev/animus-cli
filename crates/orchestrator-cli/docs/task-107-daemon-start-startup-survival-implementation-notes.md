# TASK-107 Implementation Notes: Detached Daemon Startup Survival Validation

## Phase Context
- Workflow phase: `requirements`
- Workflow ID: `82e79c48-3002-4cf0-a798-2e41d5ea2e8a`
- Task: `TASK-107`

## Purpose
Translate TASK-107 requirements into a minimal, deterministic implementation
slice that eliminates false-positive autonomous daemon starts by validating
subprocess liveness after spawn and surfacing startup log diagnostics on early
crash.

## Non-Negotiable Constraints
- Success must be emitted only after startup probe passes.
- Probe window must be deterministic and bounded to 3-5 seconds.
- Startup-failure diagnostics must include bounded log-tail context.
- Registry state must not retain dead daemon pids.
- Keep non-autonomous start behavior unchanged.
- Keep MCP behavior contractually aligned with CLI output.
- Do not manually edit `.ao/*.json`.

## Proposed Change Surface

### 1) Detached Spawn Result and Startup Probe
Primary file: `crates/orchestrator-cli/src/services/runtime/runtime_daemon.rs`

Planned changes:
- Introduce a startup probe helper for autonomous start, for example:
  - `wait_for_daemon_startup_probe(pid, timeout: Duration) -> bool`.
- Use a deterministic probe timeout constant in the 3-5 second range.
- In `DaemonCommand::Start` autonomous branch:
  - spawn subprocess,
  - run probe,
  - only persist registry pid and return success if probe passes.

Behavioral requirement:
- dead-before-probe-complete => error path, not success path.

### 2) Startup Log Capture and Failure Diagnostics
Primary file: `runtime_daemon.rs`

Planned changes:
- Ensure detached `daemon run` startup output is written to a deterministic
  task-scoped startup log file (instead of null stdio).
- Add helper to read a bounded log tail for error reporting, for example:
  - `read_startup_log_tail(path, max_lines)`.
- On probe failure, include in error message:
  - startup log path,
  - tail snippet or deterministic fallback message when unavailable.

Operational constraint:
- log-tail capture must never panic or emit unbounded output.

### 3) Registry Consistency Rules
Primary files:
- `runtime_daemon.rs`
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_registry.rs`

Planned behavior:
- Do not persist spawned pid until probe success.
- On probe failure, explicitly clear registry pid for project if needed.
- Keep existing already-running detection behavior unchanged.

### 4) MCP `ao.daemon.start` Parity
Primary file: `crates/orchestrator-cli/src/services/operations/ops_mcp.rs`

Planned behavior:
- No new tool schema required if CLI start command surfaces error correctly.
- Add/extend tests to assert `ao.daemon.start` returns
  `CallToolResult::structured_error` on startup probe failure.

### 5) Regression Tests
Primary files:
- `crates/orchestrator-cli/tests/cli_e2e.rs`
- `crates/orchestrator-cli/src/services/operations/ops_mcp.rs` (test module)
- optionally `runtime_daemon.rs` unit tests if helper extraction is added.

Target assertions:
- autonomous start success path still returns stable `daemon_pid` contract.
- early-exit child path returns failure and startup-log diagnostics.
- registry pid is not retained after failure.
- MCP daemon-start call propagates failure as structured error.

## Suggested Implementation Sequence
1. Refactor detached spawn path to produce deterministic startup log artifact.
2. Add startup probe helper and wire it into autonomous `daemon start` branch.
3. Update failure path to include log-tail diagnostics and registry cleanup.
4. Add/extend CLI and MCP tests for startup failure propagation.
5. Run targeted tests and resolve regressions introduced by TASK-107.

## Validation Targets
- `cargo test -p orchestrator-cli e2e_daemon_autonomous_start_idempotent_then_stop`
- `cargo test -p orchestrator-cli ops_mcp`
- `cargo test -p orchestrator-cli runtime_daemon`

## Risks and Mitigations
- Risk: timing flakiness from probe duration in tests.
  - Mitigation: use deterministic timeout constants and test hooks/mocks where
    possible.
- Risk: log-tail output becomes too large/noisy.
  - Mitigation: fixed line cap and trimmed formatting.
- Risk: MCP tests rely on broad command execution behavior.
  - Mitigation: assert only structured success/error semantics for
    `ao.daemon.start`.
- Risk: registry state drift when startup fails quickly.
  - Mitigation: explicit pid clear path plus regression assertion.

## Deliverables for Next Phase
- Startup-survival probe integrated into autonomous daemon start path.
- Startup-failure diagnostics with log-tail context.
- Registry pid consistency on early startup failure.
- MCP daemon-start failure propagation coverage.
- Focused tests proving no false-positive startup success.
