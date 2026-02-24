# TASK-009 Implementation Notes: Destructive Confirmation and Dry-Run Alignment

## Purpose
Translate TASK-009 requirements into concrete implementation slices for
`orchestrator-cli` without expanding scope beyond destructive safety behavior.

## Non-Negotiable Constraints
- Keep all work in Rust crates under `crates/`.
- Keep `.ao` state mutations inside existing AO command-driven persistence
  flows; do not introduce manual state edits.
- Preserve `ao.cli.v1` output envelope and existing exit-code semantics.
- Keep non-destructive command behavior unchanged.

## Proposed Change Surface

### CLI argument updates
- `crates/orchestrator-cli/src/cli_types.rs`
  - add `--dry-run` to in-scope destructive commands
  - add confirmation argument(s) to commands that currently execute destructive
    actions without confirmation:
    - `TaskCommand::Delete`
    - `TaskControlCommand::Cancel`
    - `WorkflowCommand::Cancel`
    - `WorkflowPhasesCommand::Remove`

### Command handler updates
- `crates/orchestrator-cli/src/services/runtime/runtime_project_task/task.rs`
  - implement dry-run preview and confirmation enforcement for task deletion.
- `crates/orchestrator-cli/src/services/operations/ops_task_control.rs`
  - implement dry-run preview and confirmation enforcement for task cancel.
- `crates/orchestrator-cli/src/services/operations/ops_workflow.rs`
  - implement dry-run preview and confirmation enforcement for workflow cancel
    and phase removal.
- `crates/orchestrator-cli/src/services/operations/ops_git/repo.rs`
  - add dry-run preview for `push --force`.
- `crates/orchestrator-cli/src/services/operations/ops_git/worktree.rs`
  - add dry-run preview for destructive worktree operations with `--force`.

### Shared helper alignment
- `crates/orchestrator-cli/src/services/operations/ops_common.rs`
  - introduce reusable helper(s) for deterministic destructive preview payloads
    and confirmation-required error composition.
- `crates/orchestrator-cli/src/services/operations/ops_git/store.rs`
  - keep confirmation validation centralized; extend only if needed for
    cross-domain confirmation reuse.

## Implementation Sequence
1. Add CLI flags/args for in-scope destructive operations.
2. Implement dry-run preview return paths before execution branches.
3. Enforce confirmation checks for live destructive execution.
4. Normalize preview and failure output payload keys across command groups.
5. Add/update tests and run targeted validation.

## Preview Payload Guidance
Use one stable payload shape across command groups:
- `operation`
- `target`
- `destructive`
- `dry_run`
- `requires_confirmation`
- `planned_effects`
- `next_step`

Keep `planned_effects` ordering deterministic so test snapshots stay stable.

## Confirmation Guidance
- Confirmation checks should execute immediately before side-effecting calls.
- Missing confirmation should short-circuit with `CONFIRMATION_REQUIRED`.
- Confirmation must be tied to operation intent to prevent misuse across
  unrelated targets.

## Testing Plan
- Add/extend tests in:
  - `crates/orchestrator-cli/tests/cli_smoke.rs`
  - `crates/orchestrator-cli/tests/cli_e2e.rs`
- Minimum test set:
  - each in-scope command supports `--dry-run`
  - `--dry-run` produces no mutations
  - missing confirmation fails deterministically
  - valid confirmation allows execution
  - non-destructive paths remain unchanged

## Risks and Mitigations
- Risk: command-specific drift in preview output.
  - Mitigation: centralize preview builder helper.
- Risk: backwards-compatibility break in existing git force flows.
  - Mitigation: regression tests for current `confirmation_id` lifecycle.
- Risk: over-scoping into unrelated command groups.
  - Mitigation: limit changes to audited destructive paths only.
