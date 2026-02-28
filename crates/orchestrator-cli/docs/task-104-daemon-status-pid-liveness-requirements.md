# TASK-104 Requirements: Daemon PID Liveness for Status and MCP Health

## Phase
- Workflow phase: `requirements`
- Workflow ID: `2e23cb41-10dd-4430-811f-88331116dde5`
- Task: `TASK-104`
- Requirement: unlinked in current task metadata

## Objective
Make daemon status/health deterministic by verifying daemon process liveness from
a PID file under repo-scoped runtime state instead of trusting stale persisted
daemon state alone.

## Current Baseline (Implemented)

| Surface | Current location | Current behavior | Gap |
| --- | --- | --- | --- |
| CLI daemon status | `crates/orchestrator-cli/src/services/runtime/runtime_daemon.rs` (`DaemonCommand::Status`) | returns `hub.daemon().status()` directly | status does not verify daemon process PID liveness |
| Core daemon status source | `crates/orchestrator-core/src/services/daemon_impl.rs` (`FileServiceHub::status`) | marks `crashed` when runner readiness + runner PID checks fail | runner liveness is not equivalent to daemon process liveness |
| Daemon runtime lock | `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_run.rs` (`.ao/daemon.lock`) | writes lock file with current PID in project-local `.ao` | not a repo-scoped daemon PID contract used by status/health |
| Autonomous daemon PID registry | `runtime_daemon/daemon_registry.rs` (`projects.json` `daemon_pid`) | stores daemon pid in global registry entry | status path does not consume this PID; entry can be stale |
| MCP daemon health tool | `crates/orchestrator-cli/src/services/operations/ops_mcp.rs` (`ao.daemon.health`) | shells `ao daemon health` and forwards result | result has no explicit `process_alive` field |

## Scope
In scope for implementation after this requirements phase:
- Add deterministic daemon PID file contract:
  - path: `~/.ao/<repo-scope>/daemon/daemon.pid`
  - write PID when daemon runtime starts,
  - clear or replace stale PID file when lifecycle ends/restarts.
- Update daemon status resolution to validate daemon PID liveness:
  - on unix, use `kill(pid, 0)` semantics (or equivalent wrapper),
  - when persisted state says `running`/`paused` but PID is not alive, surface a
    dead-process outcome as `crashed`.
- Add health payload liveness field:
  - `process_alive: bool` (additive),
  - ensure `ao.daemon.health` MCP output includes this field.
- Add deterministic regression tests for PID-file lifecycle and stale-state
  liveness correction.

Out of scope for this task:
- Redesigning daemon state machine enums beyond using existing `crashed`.
- Removing existing registry `daemon_pid` storage.
- Reworking runner health checks unrelated to daemon-process PID liveness.
- Manual edits to `.ao/*.json`.

## Constraints
- Keep project scoping deterministic via canonical project root and
  repo-scope derivation already used for managed runtime directories.
- Keep changes additive for existing daemon health consumers:
  - existing fields remain,
  - `process_alive` is added, not replacing fields.
- Keep daemon status response type backward-compatible for existing callers:
  - status remains a `DaemonStatus` value.
- Treat stale `running`/`paused` state with dead PID as `crashed`.
- Do not require MCP-specific forked daemon-health logic; MCP should receive the
  liveness field through the same command contract.

## Functional Requirements

### FR-01: Repo-Scoped Daemon PID File Contract
- Runtime writes daemon PID to `~/.ao/<repo-scope>/daemon/daemon.pid` on startup
  of the long-lived daemon execution path.
- PID file contents are machine-parseable decimal PID text.
- PID file parent directories are created as needed.

### FR-02: PID Liveness Validation for Status
- `ao daemon status` must verify process liveness using PID from daemon PID file
  (with safe fallback behavior for missing/invalid PID).
- If daemon state is `running` or `paused` but PID is not alive, status output
  must resolve to `crashed` (dead daemon process condition).
- Operator guidance to restart should be discoverable via health payload
  metadata/logging when dead-process condition is detected.

### FR-03: Health Payload Includes Process Liveness
- `ao daemon health` output adds `process_alive` boolean.
- `process_alive` is computed from daemon PID liveness check and is not inferred
  solely from runner connectivity.
- Existing `healthy`, `status`, `runner_connected`, and other fields remain.

### FR-04: MCP Health Parity
- MCP tool `ao.daemon.health` returns `process_alive` in
  `result.process_alive`.
- No MCP-only status heuristic diverges from CLI daemon health/status semantics.

### FR-05: Deterministic Stale-State Correction
- Repeated `ao daemon status` checks with unchanged dead PID condition must
  consistently return `crashed`.
- Dead-PID detection must not falsely report `running` because of stale
  persisted state files.

### FR-06: Regression Coverage
- Add focused tests covering:
  - daemon PID file write/read path resolution,
  - dead PID forcing status to `crashed`,
  - live PID preserving `running`/`paused`,
  - health payload including `process_alive`,
  - MCP daemon health includes `process_alive`.

## Acceptance Criteria
- `AC-01`: daemon runtime writes PID to
  `~/.ao/<repo-scope>/daemon/daemon.pid` when starting.
- `AC-02`: `ao daemon status` returns `crashed` when daemon state is
  `running`/`paused` but PID is not alive.
- `AC-03`: `ao daemon status` preserves normal status when PID is alive.
- `AC-04`: `ao daemon health` includes additive `process_alive` boolean.
- `AC-05`: MCP `ao.daemon.health` includes `result.process_alive`.
- `AC-06`: regression tests cover dead/live PID status behavior and health/MCP
  liveness-field presence.

## Testable Acceptance Checklist
- `T-01`: daemon runtime tests for PID-file lifecycle helpers (write/read/clear,
  deterministic path).
- `T-02`: status tests for stale running-state + dead PID -> `crashed`.
- `T-03`: status tests for live PID -> no forced crash.
- `T-04`: health output test asserting `process_alive` key is present.
- `T-05`: MCP daemon health test asserting `result.process_alive` is present and
  boolean.

## Verification Matrix

| Requirement area | Verification method |
| --- | --- |
| FR-01 | runtime daemon PID helper tests and daemon-run lifecycle tests |
| FR-02, FR-05 | daemon status path tests with controlled PID liveness hooks |
| FR-03 | daemon health command output contract tests |
| FR-04 | `ops_mcp` tool tests for `ao.daemon.health` result shape |
| FR-06 | targeted `cargo test -p orchestrator-cli` on touched runtime + MCP modules |

## Implementation Notes Input (Next Phase)
Primary source targets:
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon.rs`
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_run.rs`
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_git_ops.rs`
  (or shared repo-scope runtime-root helper extraction)
- `crates/orchestrator-cli/src/services/operations/ops_mcp.rs`
- `crates/orchestrator-core/src/services/daemon_impl.rs` (only if minimal
  status/health integration requires core-side additive changes)

Likely supporting tests:
- daemon runtime tests under
  `crates/orchestrator-cli/src/services/runtime/runtime_daemon/`
- MCP tests in
  `crates/orchestrator-cli/src/services/operations/ops_mcp.rs`

## Deterministic Deliverables for Implementation Phase
- Repo-scoped daemon PID file lifecycle support.
- Status liveness verification that resolves stale dead-process state to
  `crashed`.
- Additive health liveness field (`process_alive`) visible in CLI and MCP.
- Focused regression tests proving dead/live PID handling and payload contract.
