# TASK-037 Requirements: LLM Auto-Resolution for Merge Gate Conflicts

## Phase
- Workflow phase: `requirements`
- Workflow ID: `b49cf4ed-f81e-4e3e-85f2-35e2abd5308b`
- Task: `TASK-037`

## Objective
When daemon merge-gate automation hits a git merge conflict for a completed task,
the daemon must run an LLM-driven recovery flow to resolve conflicts, validate
the merged tree, finalize the merge commit, and continue normal workflow/task
completion without requiring manual unblocking for recoverable conflicts.

## Current Baseline Audit

| Surface | Current location | Current behavior | Gap |
| --- | --- | --- | --- |
| Direct merge execution | `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_git_ops.rs` (`post_success_merge_push_and_cleanup`) | runs `git merge` in a merge worktree; stdout/stderr are suppressed; any non-zero status returns a generic error | conflict-specific context is not surfaced to caller |
| Merge worktree lifecycle | same function | merge worktree is always removed before returning (including merge failure) | conflict markers and merge state are discarded before any recovery attempt |
| Workflow/task reaction on auto-merge failure | `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_project_tick.rs` (`sync_task_status_for_workflow_result`) | marks workflow merge-conflict (`mark_merge_conflict`) and blocks task with `merge gate: auto-merge failed: ...` | no conflict auto-resolution step is attempted |
| Merge gate reconciliation | `reconcile_merge_gate_tasks_for_project` in same file | only checks whether task branch is already merged and unblocks to `done` | does not attempt to recover unresolved merge conflicts |
| AI recovery primitive | `attempt_ai_failure_recovery` in same file | AI-based recovery exists for workflow phase failures | no merge-conflict-specific prompt, contract, or verification flow |

## Scope
In scope for implementation after this requirements phase:
- Add a merge-conflict recovery step in daemon merge-gate logic (preferred over
  adding a new pipeline phase for this task slice).
- Detect merge conflicts as a first-class outcome distinct from generic merge
  failures.
- Preserve conflict state long enough for an LLM agent to inspect and resolve
  conflicts in the merge worktree.
- Run deterministic verification (`cargo check` minimum) before finalizing
  merge resolution.
- Finalize and push merge result using existing integration behavior (including
  outbox fallback), then continue workflow/task completion.
- Add regression coverage and daemon observability for recovery attempt
  lifecycle.

Out of scope:
- Redesigning workflow pipeline catalogs or phase plan definitions.
- Replacing existing non-merge phase AI recovery logic.
- Broad git integration redesign beyond merge-conflict recovery surface.
- Manual edits to `/.ao/*.json`.

## Constraints
- Keep behavior deterministic and repository-safe.
- Preserve existing behavior for non-conflict merge failures.
- Do not use destructive git operations (`reset --hard`, forced branch rewrites)
  in automated recovery flow.
- Keep conflict resolution scoped to daemon-managed merge worktree paths.
- Keep `auto_pr` / direct merge / outbox push behavior consistent with current
  post-success merge flow.
- Maintain current CLI/state contracts except where explicitly required for
  merge-conflict resolution support.
- Keep all `.ao` state mutations CLI/API-driven (no manual JSON edits).

## Functional Requirements

### FR-01: Structured Merge Conflict Detection
- Merge execution must distinguish:
  - success,
  - non-conflict git failure,
  - merge conflict.
- Conflict outcome must provide structured context:
  - source branch,
  - target branch,
  - merge worktree path,
  - list of unmerged/conflicted files.

### FR-02: Conflict Workspace Preservation
- On merge conflict detection, daemon must preserve the merge worktree and merge
  state (`MERGE_HEAD` + conflict markers) for recovery.
- Cleanup of merge worktree must occur only after recovery result is terminal
  (success or explicit failure path), not before the recovery attempt.

### FR-03: Automatic LLM Recovery Trigger
- Upon merge-conflict outcome, daemon must invoke an LLM recovery run
  automatically (no manual CLI step).
- Recovery prompt must include:
  - task title/description,
  - source/target branch context,
  - conflicted file list,
  - explicit required sequence: inspect conflicts, resolve, verify, finalize.
- Recovery execution `cwd` must be the merge worktree path.

### FR-04: Recovery Output Contract
- Recovery run must return a machine-parseable JSON result object.
- Contract must include at least:
  - explicit kind/tag for merge-conflict recovery output,
  - success/failure result,
  - non-empty merge commit message on success.
- Non-parseable/contract-invalid output must be treated as recovery failure.

### FR-05: Verification Gate Before Finalization
- Recovery success path must ensure:
  - no unresolved merge entries remain,
  - required validation command(s) ran successfully.
- Minimum required validation: `cargo check` in the merge worktree.
- Targeted tests (`cargo test -p ...`) may be included by recovery policy, but
  `cargo check` is mandatory baseline.

### FR-06: Merge Finalization Continuity
- After successful resolution and verification, daemon must:
  - complete merge commit,
  - execute existing push/integration continuation behavior,
  - maintain existing cleanup semantics for successful paths.

### FR-07: Workflow and Task State Consistency
- On successful conflict resolution:
  - workflow must no longer remain in merge-conflict machine state,
  - task must end in `done` (not blocked by merge gate).
- On failed recovery:
  - workflow remains merge-conflict (or equivalent explicit failure state),
  - task may be blocked with actionable `merge gate:` reason.

### FR-08: Retry Safety and Idempotency
- Daemon must avoid uncontrolled repeated recovery attempts for the same
  unresolved conflict within a single tick.
- Repeated attempts across ticks must be bounded by deterministic attempt policy
  (cooldown and/or attempt markers).

### FR-09: Observability and Auditability
- Emit daemon events for:
  - merge-conflict recovery start,
  - success,
  - failure.
- Event payload must include workflow/task IDs and conflict context summary.
- Persist recovery diagnostics (transcript or structured metadata) in existing
  run/artifact locations for postmortem inspection.

### FR-10: Regression Coverage
- Add deterministic tests for:
  - conflict classification and context extraction,
  - conflict worktree retention through recovery attempt,
  - successful auto-resolution path (state transitions + cleanup),
  - contract-invalid/failed recovery fallback behavior.

## Acceptance Criteria
- `AC-01`: Merge conflicts are surfaced as structured outcomes, not only generic
  error strings.
- `AC-02`: Merge-conflict worktree and markers remain available during recovery
  attempt.
- `AC-03`: Daemon automatically attempts LLM conflict recovery when merge
  conflict is detected.
- `AC-04`: Recovery output is validated by a strict JSON contract; invalid output
  fails safely.
- `AC-05`: Successful recovery runs `cargo check`, leaves no unmerged paths, and
  finalizes merge continuation flow.
- `AC-06`: Successful recovery clears merge-gate blocking and results in task
  completion (`done`).
- `AC-07`: Workflow merge-conflict state is resolved on success (requires API
  support if currently missing).
- `AC-08`: Failed recovery preserves safe blocking semantics with actionable
  reason and no destructive git operations.
- `AC-09`: Daemon event/audit trail captures recovery lifecycle.
- `AC-10`: Added tests for conflict path and fallback behavior pass.

## Testable Acceptance Checklist
- `T-01`: Unit test(s) for merge conflict detection and conflicted file
  extraction.
- `T-02`: Unit/integration test proving merge worktree is preserved until
  recovery attempt concludes.
- `T-03`: Integration-style test for successful conflict auto-resolution updates
  task/workflow state correctly.
- `T-04`: Test for invalid/non-JSON recovery output resulting in deterministic
  failure handling.
- `T-05`: Test for non-conflict merge failure preserving pre-existing behavior.
- `T-06`: Targeted daemon/runtime tests pass for touched modules.

## Verification Matrix

| Requirement area | Verification method |
| --- | --- |
| FR-01, FR-02 | git-ops unit tests around merge result typing and worktree retention |
| FR-03, FR-04 | daemon scheduler tests with mocked/stubbed recovery transcript parsing |
| FR-05, FR-06 | integration-style merge-flow tests asserting check+commit+push continuation |
| FR-07 | workflow/task state transition assertions after success and failure |
| FR-08 | repeat-tick/idempotency tests for bounded retries |
| FR-09 | daemon event payload assertions and artifact presence checks |
| FR-10 | targeted `cargo test -p orchestrator-cli ...` runs for touched modules |

## Implementation Notes Input (Next Phase)
Primary implementation surfaces:
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_git_ops.rs`
  - classify merge outcomes and preserve conflict workspace for recovery path.
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_project_tick.rs`
  - trigger and orchestrate merge-conflict recovery; maintain task/workflow state.
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler.rs`
  - expose any new helper wiring between project-tick ops and git ops.
- `crates/orchestrator-cli/src/shared/task_generation.rs` (or adjacent shared
  runner helper surface)
  - add/reuse runner invocation helper that supports merge-worktree `cwd` and
    recovery prompt contract handling.
- `crates/orchestrator-core/src/services.rs`
- `crates/orchestrator-core/src/services/workflow_impl.rs`
- `crates/orchestrator-core/src/workflow/lifecycle_executor.rs`
  - add explicit workflow merge-conflict resolution API wiring if needed.

Test surfaces likely required:
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler.rs` tests
- new/expanded tests around daemon git ops merge behavior
- `crates/orchestrator-core/src/workflow/tests.rs` and workflow service tests if
  service API expands

## Deterministic Deliverables for Implementation Phase
- Structured merge-conflict outcome and context propagation.
- Automatic LLM-driven recovery attempt in merge gate path.
- Required verification gate before merge finalization.
- Correct workflow/task state transitions for success and failure.
- Regression tests and daemon event/audit evidence for conflict recovery flow.
