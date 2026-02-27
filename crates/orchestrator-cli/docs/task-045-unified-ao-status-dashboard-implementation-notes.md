# TASK-045 Implementation Notes: Unified `ao status` Dashboard Command

## Phase Context
- Workflow phase: `requirements`
- Workflow ID: `14d8d75d-e8e2-483e-9ced-9a1f0e9fe029`
- Task: `TASK-045`

## Purpose
Translate TASK-045 into a minimal, deterministic implementation slice that
adds one `ao status` command covering daemon health, active work, task
rollups, recent outcomes, and optional CI status.

## Non-Negotiable Constraints
- Keep implementation read-only (no state mutation side effects).
- Keep output deterministic and project-scoped.
- Preserve `ao.cli.v1` envelope behavior for `--json`.
- Keep degraded sections non-fatal (especially CI and daemon-connectivity edge
  cases).
- Do not manually edit `.ao/*.json`.

## Proposed Change Surface

### 1) CLI Command Plumbing
- `crates/orchestrator-cli/src/cli_types.rs`
  - add top-level `Command::Status` variant (no subcommand args required).
- `crates/orchestrator-cli/src/main.rs`
  - route `Command::Status` to a new status handler.

### 2) New Status Operations Module
- Add `crates/orchestrator-cli/src/services/operations/ops_status.rs`.
- Wire exports in `crates/orchestrator-cli/src/services/operations.rs`.
- Implement `handle_status(hub, project_root, json) -> Result<()>`.

### 3) Snapshot DTOs (Serializable)
- Add strongly typed DTOs for deterministic serialization:
  - `StatusDashboard`,
  - `DaemonStatusSlice`,
  - `ActiveAgentsSlice`,
  - `TaskSummarySlice`,
  - `RecentCompletionEntry`,
  - `RecentFailureEntry`,
  - `CiStatusSlice`.
- Include payload schema marker (for example `ao.status.v1`) inside `data`.

### 4) Data Collection Helpers
- `collect_daemon_slice`:
  - use `hub.daemon().health()` and derive `running` boolean from daemon status.
- `collect_active_agents_slice`:
  - count from daemon health,
  - running-workflow attribution from `hub.workflows().list()` joined to
    `hub.tasks().list()`.
- `collect_task_summary_slice`:
  - use `hub.tasks().statistics()`,
  - compute `done` from `by_status["done"]` to avoid cancelled inflation.
- `collect_recent_completions`:
  - from task list, `status == done`, `completed_at` present,
  - sort desc by timestamp then task id, limit 5.
- `collect_recent_failures`:
  - from workflow list, `status == failed`,
  - find failed phase from phase list/current phase fallback,
  - sort desc by failure timestamp then workflow id, limit 3.
- `collect_ci_status`:
  - probe `gh --version`,
  - when available, fetch latest run metadata for repo root,
  - convert lookup failures into CI-slice status, not hard command error.

### 5) Rendering Strategy
- JSON mode:
  - emit `StatusDashboard` through `print_value` (enveloped by `ao.cli.v1`).
- Non-JSON mode:
  - render a concise sectioned dashboard with fixed ordering and labels.
  - keep field names aligned with JSON payload keys to simplify operator/MCP
    cross-reference.

## Suggested Implementation Sequence
1. Add command variant and main dispatch route.
2. Create status module with DTOs and aggregation helpers.
3. Implement task/workflow sorting + limit logic with unit tests.
4. Add CI lookup helper with unavailable/error fallbacks.
5. Add non-JSON renderer and JSON payload output.
6. Run targeted tests and resolve regressions.

## Testing Strategy
- Module tests (`ops_status.rs`):
  - completion/failure ordering and truncation,
  - done-count semantics,
  - active-agent attribution fallback behavior.
- CLI tests (`crates/orchestrator-cli/tests/`):
  - command parse/dispatch for `ao status`,
  - `--json` payload shape assertion.
- CI helper tests:
  - mock unavailable `gh`,
  - mock valid latest run payload,
  - mock command failure and malformed payload.

## Validation Targets
- `cargo test -p orchestrator-cli ops_status`
- `cargo test -p orchestrator-cli cli_smoke -- --nocapture`
- `cargo test -p orchestrator-cli cli_e2e -- --nocapture`

## Risks and Mitigations
- Risk: active-agent count and running-workflow attribution diverge.
  - Mitigation: preserve raw count and emit explicit unknown-attribution slots
    when needed.
- Risk: CI calls add latency or brittle failures.
  - Mitigation: short timeout and non-fatal CI slice fallback.
- Risk: done-count regression by reusing terminal count.
  - Mitigation: explicit done status extraction and targeted unit test.
- Risk: unstable ordering across runs.
  - Mitigation: explicit sort keys with deterministic tie-breakers.

## Deliverables for Next Phase
- New `ao status` command path wired into CLI.
- Deterministic status snapshot DTO and renderer.
- Graceful CI integration behavior with stable fallback states.
- Focused regression tests for aggregation and output contracts.
