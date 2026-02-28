# TASK-104 Implementation Notes: Daemon PID Liveness in Status/Health

## Phase Context
- Workflow phase: `requirements`
- Workflow ID: `2e23cb41-10dd-4430-811f-88331116dde5`
- Task: `TASK-104`

## Purpose
Translate TASK-104 into a minimal, deterministic implementation slice that:
- records daemon PID in repo-scoped runtime state,
- validates daemon-process liveness for status resolution, and
- exposes `process_alive` in daemon health (including MCP consumers).

## Non-Negotiable Constraints
- Keep changes scoped to daemon runtime + MCP health/status surfaces.
- Keep status output backward-compatible as `DaemonStatus`.
- Keep daemon health changes additive (`process_alive` only; no field removal).
- Use deterministic repo-scoped runtime paths under `~/.ao/<repo-scope>/...`.
- Do not manually edit `.ao/*.json`.

## Proposed Change Surface

### 1) Add Shared Daemon PID File Helpers
- Primary target:
  - `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_run.rs`
- Add helpers for:
  - resolving daemon PID path:
    - `~/.ao/<repo-scope>/daemon/daemon.pid`,
  - reading/parsing PID file,
  - writing current PID atomically,
  - clearing stale pid file on shutdown.
- Prefer reusing existing repo-scope root helpers (or extract one shared helper)
  rather than duplicating scope hashing/path logic.

### 2) Persist PID on Daemon Runtime Start
- Target:
  - `daemon_run.rs` (`acquire_daemon_run_guard`, `Drop` for guard)
- On successful daemon-run guard acquisition:
  - write pid file for current process.
- On guard drop:
  - clear pid file if it still matches current PID.
- Keep existing lock-file behavior (`.ao/daemon.lock`) intact.

### 3) Add Daemon Process Snapshot for Status/Health
- Primary target:
  - `crates/orchestrator-cli/src/services/runtime/runtime_daemon.rs`
- Introduce helper that returns:
  - `daemon_pid: Option<u32>`,
  - `process_alive: bool`.
- Liveness check:
  - unix uses existing `kill -0` semantics (`is_process_alive` wrapper),
  - non-unix uses current platform-specific fallback already in module.

### 4) Update `daemon status` Resolution
- Target:
  - `runtime_daemon.rs` (`DaemonCommand::Status` branch)
- Behavior:
  - obtain baseline `daemon.status()` from hub,
  - if baseline is `running`/`paused` and `process_alive == false`, return
    `DaemonStatus::Crashed`.
- Optional additive operator guidance:
  - emit restart recommendation in logs/health metadata, while keeping status
    return type unchanged.

### 5) Update `daemon health` Payload
- Target:
  - `runtime_daemon.rs` (`DaemonCommand::Health` branch)
- Build output object that preserves existing health fields and adds:
  - `process_alive: bool`.
- Ensure JSON envelope remains `ao.cli.v1` via existing `print_value` path.

### 6) MCP Health Parity
- Target:
  - `crates/orchestrator-cli/src/services/operations/ops_mcp.rs`
- Because MCP wraps CLI `daemon health`, no schema fork is required.
- Add/adjust MCP tests that assert `ao.daemon.health` includes
  `result.process_alive`.

## Suggested Implementation Sequence
1. Add PID-path and file-lifecycle helpers with unit tests.
2. Wire PID write/clear into daemon run guard lifecycle.
3. Add process snapshot helper in runtime daemon command handler.
4. Update status crash-on-dead-pid behavior.
5. Update health payload shaping with additive `process_alive`.
6. Add MCP + runtime tests, then run targeted test commands.

## Validation Targets
- `cargo test -p orchestrator-cli runtime_daemon::daemon_run`
- `cargo test -p orchestrator-cli runtime_daemon`
- `cargo test -p orchestrator-cli services::operations::ops_mcp`
- Optional broader safety pass:
  - `cargo test -p orchestrator-cli`

## Risks and Mitigations
- Risk: duplicated repo-scope path derivation causes path drift.
  - Mitigation: reuse existing repo-scope helper or centralize new helper in one
    daemon runtime module.
- Risk: stale/missing pid file causes false crash reports.
  - Mitigation: deterministic fallback handling and explicit tests for
    missing/invalid PID states.
- Risk: status output contract break for existing consumers.
  - Mitigation: keep status as `DaemonStatus` enum value only.
- Risk: MCP behavior diverges from CLI.
  - Mitigation: rely on shared CLI health output contract and assert via MCP
    tests.

## Deliverables for Next Phase
- Repo-scoped daemon pid file write/read/clear lifecycle.
- Dead PID detection that forces stale running/paused status to `crashed`.
- `ao daemon health` additive `process_alive` field, surfaced through MCP.
- Focused regression tests for PID liveness behavior and output contracts.
