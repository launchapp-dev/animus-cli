# TASK-049 Implementation Notes: Stale In-Progress Task Detection and Alerting

## Phase Context
- Workflow phase: `requirements`
- Workflow ID: `eb92de20-d8f2-4695-8b46-dce36c101085`
- Task: `TASK-049`

## Purpose
Translate TASK-049 requirements into a minimal, deterministic implementation
slice that:
- flags stale `in-progress` tasks by `updated_at` age (default `24h`),
- exposes stale details in `ao task stats`,
- emits stale counters in daemon queue telemetry for alert consumers.

## Non-Negotiable Constraints
- Keep stale detection read-only and deterministic.
- Keep existing `task stats` fields and daemon queue fields intact (additive).
- Keep threshold parsing strict (`> 0`) with clear validation errors.
- Keep changes Rust-only within workspace crates.
- Do not manually edit `.ao/*.json`.

## Proposed Change Surface

### 1) CLI Argument Plumbing
- `crates/orchestrator-cli/src/cli_types/task_types.rs`
  - change `TaskCommand::Stats` to carry `TaskStatsArgs`.
  - add:
    - `--stale-threshold-hours <HOURS>` (default `24`, positive integer).

- `crates/orchestrator-cli/src/cli_types/daemon_types.rs`
  - add `stale_threshold_hours` to:
    - `DaemonRunArgs`,
    - `DaemonStartArgs`.
  - default `24`, parsed as positive integer.

- `crates/orchestrator-cli/src/services/runtime/runtime_daemon.rs`
  - forward `DaemonStartArgs.stale_threshold_hours` into detached
    `daemon run` invocation (`spawn_autonomous_daemon_run`).

### 2) Shared Stale Detection Helper
- Add a small reusable helper module in runtime services (single source of
  truth) for stale detection, for example:
  - `crates/orchestrator-cli/src/services/runtime/stale_in_progress.rs`
- Proposed helper outputs:
  - `StaleInProgressEntry` (`task_id`, `title`, `updated_at`, `age_hours`)
  - `StaleInProgressSummary` (`threshold_hours`, `count`, `entries`)
- Helper behavior:
  - filter only `TaskStatus::InProgress`,
  - compare `Utc::now()` against `metadata.updated_at`,
  - deterministic sort:
    - `updated_at` ascending,
    - then task id ascending.

### 3) `ao task stats` Integration
- `crates/orchestrator-cli/src/services/runtime/runtime_project_task/task.rs`
  - in stats handler:
    - fetch base stats via `tasks.statistics()`,
    - fetch tasks list via `tasks.list()`,
    - compute stale summary with threshold from CLI args,
    - emit merged JSON payload:
      - existing `TaskStatistics` fields unchanged,
      - additive `stale_in_progress` object.
- Keep `--json` envelope behavior unchanged (`ao.cli.v1`).

### 4) Daemon Tick Summary Integration
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler.rs`
  - extend `ProjectTickSummary` with additive stale fields:
    - `stale_in_progress_count`,
    - `stale_in_progress_threshold_hours`,
    - `stale_in_progress_task_ids`.

- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_project_tick.rs`
  - compute stale summary from tick task snapshot using
    `args.stale_threshold_hours`,
  - populate new stale fields in `ProjectTickSummary`.

### 5) Daemon Queue Event Integration
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_run.rs`
  - include new stale fields in emitted `queue` event payload.
  - keep all pre-existing queue keys unchanged.

## Suggested Implementation Sequence
1. Add new CLI args and parser coverage for task stats + daemon run/start.
2. Add shared stale-detection helper and unit tests.
3. Wire helper into `task stats` output payload.
4. Extend daemon tick summary and queue event payload with stale fields.
5. Update/extend JSON contract and daemon runtime tests.
6. Run targeted tests and fix regressions introduced by TASK-049 changes.

## Testing Plan
- CLI parsing tests (`crates/orchestrator-cli/src/cli_types/mod.rs`):
  - new threshold flags parse,
  - invalid `0` is rejected.
- Stale helper unit tests:
  - threshold boundary behavior,
  - status filtering,
  - deterministic ordering.
- JSON contract tests (`crates/orchestrator-cli/tests/cli_json_contract.rs`):
  - assert `data.stale_in_progress.count` exists for `task stats`.
- Daemon runtime tests:
  - queue payload includes new stale fields,
  - forwarded threshold value appears in summary data.
- Guard test:
  - stale detection path does not mutate task state.

## Validation Targets
- `cargo test -p orchestrator-cli cli_types::tests`
- `cargo test -p orchestrator-cli services::runtime::runtime_project_task`
- `cargo test -p orchestrator-cli runtime_daemon::daemon_scheduler`
- `cargo test -p orchestrator-cli runtime_daemon::daemon_run`
- `cargo test -p orchestrator-cli cli_json_contract`

## Risks and Mitigations
- Risk: divergence between task stats and daemon stale calculations.
  - Mitigation: shared helper module and shared tests.
- Risk: accidental breaking change to stats/queue consumers.
  - Mitigation: additive-only payload changes and explicit contract assertions.
- Risk: noisy stale telemetry in daemon loops.
  - Mitigation: keep payload compact (`task_ids` list) and deterministic.
- Risk: threshold ambiguity across commands.
  - Mitigation: explicit default in CLI args and mirrored test assertions.

## Deliverables for Next Phase
- New threshold flags for `task stats` and daemon run/start.
- Shared stale detector used by both task stats and daemon tick logic.
- Additive stale fields in task stats output and daemon queue telemetry.
- Targeted regression tests covering parser, detector, and payload contracts.
