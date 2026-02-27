# TASK-054 Requirements: Prevent Premature `completed_at` in MergeConflict

## Phase
- Workflow phase: `requirements`
- Workflow ID: `4cfec7e7-74ad-4ae6-a4e5-f291227c92f3`
- Task: `TASK-054`

## Objective
When a workflow enters `MergeConflict`, it must not carry a completion timestamp
until the workflow reaches a true terminal completion outcome. This keeps merge
conflict workflows from being misclassified as completed while recovery is still
required.

## Current Baseline Audit

| Surface | Current location | Current behavior | Gap |
| --- | --- | --- | --- |
| Merge-conflict entry | `crates/orchestrator-core/src/workflow/lifecycle_executor.rs` (`mark_merge_conflict`) | requires `status == Completed`, sets `machine_state = MergeConflict`, writes `failure_reason`, sets `completed_at = Some(Utc::now())` | non-terminal conflict state receives completion timestamp |
| Merge-conflict resolution | `crates/orchestrator-core/src/workflow/lifecycle_executor.rs` (`resolve_merge_conflict`) | transitions to `Completed`, clears `failure_reason`, sets `completed_at` | resolution behavior is valid, but pre-resolution timestamp semantics are wrong |
| Built-in workflow machine terminal metadata | `crates/orchestrator-core/src/state_machines/schema.rs` (`builtin_state_machines_document`) | `terminal_states` includes `MergeConflict` | contradicts task intent: merge conflict is recoverable and should be non-terminal |
| Repo state-machine JSON | `.ao/state/state-machines.v1.json` | workflow terminal states omit `merge-conflict`; merge-conflict transitions absent | runtime may rely on lifecycle executor fallback assignment; requirements must preserve compatibility |
| Regression coverage | `crates/orchestrator-core/src/workflow/tests.rs` | merge-conflict tests assert state/failure-reason transitions, but not `completed_at` semantics | timestamp regression is currently unguarded |

## Problem Statement
`mark_merge_conflict` currently writes a completion timestamp while the workflow
is in a recoverable conflict state. This creates incorrect completion semantics
for status/history surfaces that use `completed_at`.

## Decision for Implementation
- Entering `MergeConflict` must clear workflow completion timestamp
  (`completed_at = None`).
- Resolving `MergeConflict` back to `Completed` remains responsible for setting
  `completed_at`.
- Built-in workflow machine terminal metadata should treat `MergeConflict` as
  non-terminal.
- Keep this task scoped to completion semantics and terminal-state metadata;
  avoid broader workflow status model changes unless required for correctness.

## Scope
In scope for implementation after this requirements phase:
- Update merge-conflict lifecycle behavior to clear `completed_at` on conflict
  entry.
- Keep `completed_at` assignment on terminal completion paths (`Completed`,
  `Failed`, `Cancelled`) unchanged unless directly impacted by this fix.
- Align built-in workflow machine terminal-state list so `MergeConflict` is not
  terminal.
- Add regression tests for merge-conflict timestamp semantics and terminal
  metadata.

Out of scope:
- Redesigning `WorkflowStatus` enum semantics for merge-conflict workflows.
- Editing repository `.ao` JSON files directly as manual state mutation.
- Broad workflow/pipeline refactors outside merge-conflict completion semantics.

## Constraints
- Keep behavior deterministic and repository-safe.
- Preserve existing merge-conflict state transitions (`Completed` ->
  `MergeConflict` -> `Completed`).
- Avoid destructive recovery shortcuts.
- Maintain compatibility with projects using JSON-loaded state-machine metadata
  where merge-conflict transitions may be absent.

## Functional Requirements

### FR-01: Clear Completion Timestamp on MergeConflict Entry
- When `mark_merge_conflict` is applied, workflow `completed_at` must be
  explicitly set to `None`.

### FR-02: Preserve Terminal Completion Timestamp Assignment
- `completed_at` must continue to be set on terminal completion paths:
  - workflow completion (`mark_current_phase_success` final phase),
  - workflow failure (`mark_current_phase_failed`),
  - workflow cancellation (`cancel`),
  - merge-conflict resolution to completed (`resolve_merge_conflict`).

### FR-03: Built-in Terminal Metadata Parity
- Built-in workflow machine terminal-state metadata must exclude
  `WorkflowMachineState::MergeConflict`.

### FR-04: Backward-Compatible MergeConflict State Assignment
- Lifecycle executor fallback behavior that forces `machine_state =
  MergeConflict` when transition metadata is missing must remain intact.

### FR-05: Regression Test Coverage
- Tests must verify:
  - merge-conflict entry clears `completed_at`,
  - merge-conflict resolution sets `completed_at`,
  - built-in machine marks `MergeConflict` as non-terminal.

## Acceptance Criteria
- `AC-01`: A workflow that enters `MergeConflict` has `completed_at == None`.
- `AC-02`: A workflow resolved from `MergeConflict` to `Completed` has
  `completed_at.is_some()`.
- `AC-03`: `mark_current_phase_failed` and `cancel` still set `completed_at`.
- `AC-04`: Built-in compiled workflow machine reports
  `is_terminal(MergeConflict) == false`.
- `AC-05`: Existing merge-conflict transition behavior remains functional.

## Testable Acceptance Checklist
- `T-01`: Update/add lifecycle tests in
  `crates/orchestrator-core/src/workflow/tests.rs` for merge-conflict
  `completed_at` behavior.
- `T-02`: Add/extend state-machine tests validating
  `CompiledWorkflowMachine::is_terminal(WorkflowMachineState::MergeConflict)`
  is false for built-ins.
- `T-03`: Run targeted tests:
  - `cargo test -p orchestrator-core workflow::tests -- --nocapture`
  - `cargo test -p orchestrator-core state_machine_parity -- --nocapture`

## Verification Matrix

| Requirement area | Verification method |
| --- | --- |
| FR-01, FR-02 | lifecycle unit tests asserting `completed_at` before and after merge-conflict transitions |
| FR-03 | state-machine metadata assertion against built-in compiled machine |
| FR-04 | existing merge-conflict transition tests plus fallback state assignment checks |
| FR-05 | targeted `orchestrator-core` test run for touched modules |

## Implementation Notes Input (Next Phase)
Primary implementation surfaces:
- `crates/orchestrator-core/src/workflow/lifecycle_executor.rs`
  - clear `completed_at` in `mark_merge_conflict`.
- `crates/orchestrator-core/src/state_machines/schema.rs`
  - remove `WorkflowMachineState::MergeConflict` from built-in
    `terminal_states`.
- `crates/orchestrator-core/src/workflow/tests.rs`
  - add explicit assertions for merge-conflict timestamp behavior.
- `crates/orchestrator-core/src/state_machines/*` tests
  - add/extend terminal-metadata regression checks.

## Deterministic Deliverables for Implementation Phase
- Merge-conflict entry no longer produces premature completion timestamp.
- Built-in workflow machine terminal metadata reflects merge-conflict as
  non-terminal.
- Regression tests prevent timestamp/terminal-state drift.
