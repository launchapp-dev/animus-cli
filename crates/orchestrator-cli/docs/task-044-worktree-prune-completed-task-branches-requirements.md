# TASK-044 Requirements: Prune Completed Task Worktrees

## Phase
- Workflow phase: `requirements`
- Workflow ID: `6f99338b-8b02-4529-b78f-37537757c609`
- Task: `TASK-044`

## Objective
Add a deterministic, repository-safe prune flow that cleans stale daemon-managed
task worktrees under `~/.ao/<scope>/worktrees/` when their task is terminal
(`done` or `cancelled`), with preview support and optional remote branch
deletion.

## Current Baseline Audit

| Surface | Current location | Current behavior | Gap |
| --- | --- | --- | --- |
| Git worktree command surface | `crates/orchestrator-cli/src/cli_types.rs` (`GitWorktreeCommand`) | supports `create/list/get/remove/pull/push/sync/sync-status` | no prune command |
| Worktree command handler | `crates/orchestrator-cli/src/services/operations/ops_git/worktree.rs` | can list/remove one worktree at a time; no task-aware filtering | no batch cleanup for completed tasks |
| Worktree discovery parser | `crates/orchestrator-cli/src/services/operations/ops_git/store.rs` | parses `git worktree list --porcelain` into path/head/branch | no linkage to task status |
| Daemon post-success cleanup | `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_git_ops.rs` (`cleanup_task_worktree_if_enabled`) | removes only the current task worktree after successful merge path | no backlog pruning of older completed task worktrees |
| Daemon config CLI | `crates/orchestrator-cli/src/cli_types.rs`, `runtime_daemon.rs` (`DaemonConfigArgs`) | supports auto-merge / auto-pr / auto-commit-before-merge | no auto-prune toggle |
| Core daemon config schema | `crates/orchestrator-core/src/daemon_config.rs` | supports persisted merge-related toggles | no persisted auto-prune setting |

## Scope
In scope for implementation after this requirements phase:
- Add a new CLI command to prune completed-task worktrees:
  - `ao git worktree prune ...`
- Command must:
  - enumerate worktrees,
  - identify entries linked to tasks in `done` or `cancelled`,
  - remove both git worktree entry and on-disk directory,
  - optionally delete remote branch for each pruned task branch,
  - support `--dry-run` with deterministic preview output.
- Add a daemon option to auto-run prune after successful merge completion.
- Add targeted tests for command behavior and daemon integration.

Out of scope:
- Redesigning worktree naming conventions.
- Pruning non-task worktrees by heuristic-only matching.
- Auto-deleting local branches outside current merge/cleanup behavior.
- Manual edits to `.ao/*.json`.

## Constraints
- Keep cleanup scoped to daemon-managed task worktrees under the repository
  worktree scope root.
- Never prune the primary repository worktree.
- Only prune when linked task status is terminal (`done` or `cancelled`).
- `--dry-run` must be side-effect free:
  - no git mutations,
  - no directory removals,
  - no task state writes.
- Preserve existing JSON envelope behavior (`ao.cli.v1`) and stable
  deterministic ordering for preview/result lists.
- Keep `.ao` mutations API-driven through existing services (`hub.tasks()`),
  not manual file writes.
- Remote branch deletion must be opt-in and safe-gated.

## Functional Requirements

### FR-01: New Prune Command Surface
- Add `GitWorktreeCommand::Prune` with args supporting at minimum:
  - `--repo <REPO>`
  - `--dry-run`
  - optional remote branch cleanup toggle (for example
    `--delete-remote-branch`)
  - remote selection for branch delete (default `origin`)
  - confirmation token input for live destructive execution.

### FR-02: Candidate Enumeration and Filtering
- Prune flow must read `git worktree list --porcelain` via existing worktree
  parsing helpers.
- For each worktree entry, determine linked task by deterministic lookup using
  task metadata (`worktree_path`, `branch_name`, and canonical task worktree
  naming conventions as fallback).
- Candidate is prunable only if linked task status is `done` or `cancelled`.
- Entries with no resolvable task link must be reported as skipped with reason.

### FR-03: Safe Scope Guardrails
- Prune flow must skip and report:
  - primary repository worktree,
  - entries outside managed worktree root,
  - entries linked to non-terminal tasks,
  - entries with missing prerequisite metadata for requested action.

### FR-04: Local Prune Execution Contract
- For each prunable candidate in live mode:
  - remove git worktree registration,
  - remove worktree directory if still present,
  - clear task `worktree_path` in persisted task state when it matches removed
    path.
- Execution must be deterministic (stable candidate order).
- Command should return per-entry success/failure status and a summary count.

### FR-05: Optional Remote Branch Deletion
- When remote deletion flag is enabled, prune must attempt remote branch delete
  for each pruned candidate with a resolvable task branch.
- Branch deletion must be skipped for non-task/protected targets by safety rule
  (for example, reject obvious protected branch names and non-task branch
  patterns) and reported with reason.
- Remote delete result must be included per candidate without hiding failures.

### FR-06: Dry-Run Preview Contract
- `--dry-run` output must include:
  - operation metadata (`operation`, `dry_run`, `destructive`),
  - candidate list with planned local/remote actions,
  - skipped list with reasons,
  - deterministic counts,
  - explicit next-step guidance for live execution.
- Live-mode destructive operations must remain confirmation-gated.

### FR-07: Idempotency and Failure Handling
- Running prune repeatedly after successful cleanup should produce zero-op
  results (idempotent behavior).
- One candidate failure must not prevent processing remaining candidates;
  aggregate result must surface partial failures clearly.

### FR-08: Daemon Auto-Prune Option
- Add daemon configuration support for auto-pruning completed task worktrees
  after successful merge flow completion.
- Support persisted config (`ao daemon config`) and run/start overrides.
- Auto-prune execution from daemon must:
  - default to local prune only (no remote branch deletion),
  - not regress task completion when prune fails (best-effort with logging).

### FR-09: Observability
- Emit structured daemon event(s) when auto-prune runs, including:
  - attempted candidate count,
  - pruned count,
  - skipped count,
  - failed count.
- CLI prune command output must expose equivalent summary data.

### FR-10: Regression Coverage
- Add deterministic tests for:
  - candidate selection by terminal task status,
  - dry-run no-mutation contract,
  - local prune action + task metadata cleanup,
  - optional remote branch deletion behavior,
  - daemon auto-prune toggle and non-blocking failure behavior.

## Acceptance Criteria
- `AC-01`: `ao git worktree prune` exists and supports dry-run preview.
- `AC-02`: Prune only targets worktrees linked to `done`/`cancelled` tasks.
- `AC-03`: Primary worktree and non-managed paths are never pruned.
- `AC-04`: Live prune removes git worktree entries and on-disk directories for
  pruned candidates.
- `AC-05`: Pruned tasks have stale `worktree_path` cleared via service-driven
  update.
- `AC-06`: Optional remote branch deletion is opt-in, safety-gated, and
  reported per candidate.
- `AC-07`: Dry-run performs zero mutations and returns deterministic plan data.
- `AC-08`: Live prune remains confirmation-gated for destructive execution.
- `AC-09`: Daemon can auto-prune after successful merge when enabled.
- `AC-10`: Auto-prune failures do not block task completion.
- `AC-11`: CLI and daemon output include deterministic prune summary counts.
- `AC-12`: Targeted tests cover prune command and daemon auto-prune behavior.

## Testable Acceptance Checklist
- `T-01`: `cli_smoke` help coverage for `git worktree prune` flags.
- `T-02`: e2e dry-run test verifies no git/worktree/task mutation.
- `T-03`: e2e live prune test verifies local worktree removal and task metadata
  update.
- `T-04`: e2e remote deletion path test verifies opt-in behavior and skip rules.
- `T-05`: daemon runtime test verifies auto-prune invocation when enabled.
- `T-06`: daemon runtime test verifies prune failure is logged without
  regressing completion state.

## Verification Matrix

| Requirement area | Verification method |
| --- | --- |
| FR-01, FR-06 | CLI command argument and dry-run output assertions |
| FR-02, FR-03 | candidate classification tests against mixed worktree/task fixtures |
| FR-04, FR-07 | live prune integration test with partial failure + idempotent rerun |
| FR-05 | remote branch delete opt-in test and safety skip assertions |
| FR-08, FR-09 | daemon run/project tick tests with event payload checks |
| FR-10 | targeted `cargo test -p orchestrator-cli` for touched modules |

## Implementation Notes Input (Next Phase)
Primary source targets:
- `crates/orchestrator-cli/src/cli_types.rs`
- `crates/orchestrator-cli/src/services/operations/ops_git/worktree.rs`
- `crates/orchestrator-cli/src/services/operations/ops_git/store.rs`
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon.rs`
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_run.rs`
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_git_ops.rs`
- `crates/orchestrator-core/src/daemon_config.rs`

Likely test targets:
- `crates/orchestrator-cli/tests/cli_smoke.rs`
- `crates/orchestrator-cli/tests/cli_e2e.rs`
- daemon runtime tests in
  `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_run.rs`
  and/or scheduler tests.

## Deterministic Deliverables for Implementation Phase
- Task-aware `git worktree prune` command with dry-run and live execution paths.
- Optional remote branch deletion for pruned task branches.
- Daemon auto-prune toggle and post-merge invocation.
- Stable summary/reporting surfaces and focused regression tests.
