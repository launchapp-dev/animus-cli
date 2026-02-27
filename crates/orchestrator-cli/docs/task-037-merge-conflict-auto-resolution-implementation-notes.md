# TASK-037 Implementation Notes: Merge Gate Conflict Auto-Resolution

## Phase Context
- Workflow phase: `requirements`
- Workflow ID: `b49cf4ed-f81e-4e3e-85f2-35e2abd5308b`
- Task: `TASK-037`

## Purpose
Translate `TASK-037` requirements into a minimal, deterministic implementation
slice that adds LLM-assisted merge-conflict recovery to daemon merge-gate flow
without broad workflow/pipeline redesign.

## Architectural Decision
Use a merge-gate recovery step, not a new pipeline phase, for this task:
- conflict recovery is a post-phase git integration concern;
- it should execute only when merge operation fails with conflicts;
- it must run in merge worktree context with preserved git conflict state.

This keeps change scope local to daemon scheduler + git ops and avoids
expanding default phase catalogs/pipelines.

## Non-Negotiable Constraints
- Keep existing success behavior for PR/direct-merge flows intact.
- Preserve behavior for non-conflict merge failures.
- Do not destroy conflict context before recovery attempt.
- Avoid destructive git operations in automated path.
- Keep `.ao` mutation policy intact (no manual edits to `/.ao/*.json`).
- Keep recovery output machine-parseable and contract validated.

## Proposed Change Surface

### 1) Introduce structured merge outcomes in git ops
- File:
  - `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_git_ops.rs`
- Replace generic merge failure reporting with explicit outcome model:
  - success/no-op,
  - merge conflict + context,
  - other failure.
- Capture conflict context:
  - source branch,
  - target branch,
  - merge worktree path,
  - conflicted file list (`git diff --name-only --diff-filter=U`).

Recommended approach:
- migrate `post_success_merge_push_and_cleanup` return surface from `Result<bool>`
  to `Result<PostMergeOutcome>` (or equivalent typed result) so callers can
  branch deterministically without parsing error strings.

### 2) Preserve merge worktree through conflict path
- File:
  - `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_git_ops.rs`
- Adjust cleanup sequencing:
  - keep merge worktree on conflict until recovery attempt finishes;
  - run cleanup only after terminal success/failure handling.
- Ensure helper cleanup path remains idempotent and safe.

### 3) Add merge-conflict recovery runner orchestration
- Primary file:
  - `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_project_tick.rs`
- Optional extraction target:
  - `daemon_scheduler_merge_conflict_recovery.rs` (if file size/clarity
    warrants extraction)

Add flow:
1. detect `PostMergeOutcome::Conflict`.
2. build deterministic recovery prompt with task + branch + conflict context.
3. run LLM recovery against runner with `cwd = merge_worktree_path`.
4. parse strict JSON response and enforce contract.
5. verify required checks (minimum `cargo check`) completed and successful.
6. continue merge finalization/push path on success.

### 4) Add runner helper for merge recovery context
- Candidate files:
  - `crates/orchestrator-cli/src/shared/task_generation.rs`
  - or new shared daemon helper near scheduler modules

Need helper that supports:
- arbitrary `cwd` (merge worktree),
- explicit runtime contract/prompt context for merge recovery,
- robust transcript capture + JSON payload extraction.

Do not reuse task-generation helper unchanged if it constrains tool behavior
incompatible with git-edit/verification requirements.

### 5) Add explicit workflow merge-conflict resolution API if needed
- Files:
  - `crates/orchestrator-core/src/services.rs`
  - `crates/orchestrator-core/src/services/workflow_impl.rs`
  - `crates/orchestrator-core/src/workflow/lifecycle_executor.rs`

Current surface includes `mark_merge_conflict` but no workflow service method
to resolve it. Add `resolve_merge_conflict` API wiring so successful automated
resolution can clear merge-conflict machine state consistently.

### 6) State transition integration
- File:
  - `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_project_tick.rs`

Update `sync_task_status_for_workflow_result` and merge gate reconciliation so:
- success path clears conflict state and marks task `done`,
- failure path blocks with actionable merge-gate reason,
- repeat attempts are bounded and deterministic.

### 7) Observability and audit trail
- File:
  - `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_project_tick.rs`
  - plus existing artifact/event helpers as needed

Emit events for recovery start/success/failure and persist recovery metadata
(transcript summary, conflict files count, validation outcomes).

## Suggested Implementation Sequence
1. Add typed merge outcome + conflict context in git ops.
2. Adjust merge worktree cleanup ordering for conflict preservation.
3. Add merge recovery runner + JSON contract parser.
4. Integrate recovery path into project tick state sync.
5. Add workflow service API support for conflict resolution state clearing.
6. Add/expand tests for conflict success/failure branches.
7. Run targeted tests and checks; tighten error messages and cleanup behavior.

## Validation Targets
- `cargo check -p orchestrator-cli -p orchestrator-core`
- targeted tests (names illustrative, align to implemented modules):
  - `cargo test -p orchestrator-cli runtime_daemon::daemon_scheduler`
  - `cargo test -p orchestrator-core workflow::tests`
  - `cargo test -p orchestrator-core services::tests`

If conflict-flow tests require git integration setup, include deterministic temp
repo fixtures with explicit branch/conflict creation.

## Risks and Mitigations
- Risk: merge conflict not reliably differentiated from other git errors.
  - Mitigation: explicit unmerged-file detection + typed outcome enum.
- Risk: recovery run produces unparsable or unsafe output.
  - Mitigation: strict JSON contract, reject invalid payloads, fail safe.
- Risk: cleanup order leaks temporary merge worktrees.
  - Mitigation: explicit terminal cleanup helper with idempotent calls.
- Risk: workflow remains in merge-conflict despite successful merge.
  - Mitigation: add explicit workflow resolve API and assert in tests.
- Risk: repeated daemon ticks trigger runaway retries.
  - Mitigation: bounded retry markers/cooldown and deterministic attempt policy.

## Out-of-Scope Reminder
- No manual edits to `.ao` JSON state/config files.
- No redesign of workflow phase catalogs for this task slice.
- No broad refactor of unrelated daemon scheduler concerns.
