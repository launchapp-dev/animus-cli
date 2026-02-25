# TASK-002 Implementation Notes: CLI Help and Error Message Polish

## Purpose
Translate `TASK-002` requirements into concrete implementation slices for
`orchestrator-cli` with strict scope control and no behavior regressions in
core command execution.

## Non-Negotiable Constraints
- Keep command names and primary flag names backward compatible.
- Preserve `ao.cli.v1` success/error envelope shape.
- Preserve existing exit-code classification semantics.
- Keep destructive operations side-effect free when `--dry-run` is set.
- Do not manually edit `.ao` JSON state files.

## Proposed Change Surface

### Help and CLI metadata
- `crates/orchestrator-cli/src/cli_types.rs`
  - add `about` metadata for scoped command groups:
    - `task`
    - `task-control`
    - `workflow`
    - `requirements`
    - `git`
  - add concise argument help text for high-impact flags.
  - document precedence for `--input-json` arguments.
  - use `value_name` and help text to clarify expected formats.

### Validation and message contracts
- `crates/orchestrator-cli/src/shared/parsing.rs`
  - centralize accepted-value lists for bounded-domain parsers.
  - upgrade invalid-value errors to include:
    - domain/argument name
    - invalid value
    - accepted values
    - next-step hint
  - keep alias compatibility.

- `crates/orchestrator-cli/src/shared/output.rs`
  - keep envelope and exit code mapping unchanged.
  - align non-JSON error prefixing/wording to remain concise and actionable.

### Confirmation and dry-run output alignment
- `crates/orchestrator-cli/src/services/runtime/runtime_project_task/task.rs`
- `crates/orchestrator-cli/src/services/operations/ops_task_control.rs`
- `crates/orchestrator-cli/src/services/operations/ops_workflow.rs`
- `crates/orchestrator-cli/src/services/operations/ops_git/repo.rs`
- `crates/orchestrator-cli/src/services/operations/ops_git/worktree.rs`
- `crates/orchestrator-cli/src/services/operations/ops_common.rs` (if helper
  extraction is needed)
  - standardize dry-run payload common keys.
  - keep command-specific details while preserving stable top-level contract.
  - align `CONFIRMATION_REQUIRED` messaging structure across handlers.

## Suggested Message Contract

### Invalid-value contract
Preferred shape:
- `invalid <domain> '<value>'; expected one of: <v1>, <v2>, ...; run '<command> --help'`

Requirements:
- deterministic ordering of accepted values,
- stable punctuation for test assertions,
- no environment-dependent text.

### Confirmation-required contract
Preferred shape:
- `CONFIRMATION_REQUIRED: rerun '<command>' with <confirmation flag> <token>; use --dry-run to preview changes`

Requirements:
- include exact flag name expected by that command,
- mention preview path when supported.

### Dry-run preview contract
Shared top-level keys:
- `operation`
- `target`
- `destructive`
- `dry_run`
- `requires_confirmation`
- `planned_effects`
- `next_step`

Allow command-specific companion fields but keep shared keys stable.

## Suggested Build Sequence
1. Add/adjust command/argument help metadata in `cli_types.rs`.
2. Introduce shared invalid-value message helper(s) in `shared/parsing.rs`.
3. Align confirmation-required message phrasing paths.
4. Normalize dry-run payload key set in scoped destructive handlers.
5. Update tests for help content and error-message expectations.
6. Run targeted CLI tests and fix regressions.

## Test Plan

### Existing test files to extend
- `crates/orchestrator-cli/tests/cli_smoke.rs`
  - add scoped command help assertions.
- `crates/orchestrator-cli/tests/cli_e2e.rs`
  - assert confirmation guidance text shape.
  - assert dry-run shared keys for scoped destructive commands.

### Unit tests to expand
- `crates/orchestrator-cli/src/shared/parsing.rs` tests
  - verify accepted-values list appears in invalid-value errors.
  - verify alias values still parse correctly.

## Regression Guardrails
- Avoid broad command-surface rewrites during this task.
- Keep business logic changes minimal and message-contract focused.
- Do not modify state file schemas or workflow/task domain transitions.
- Validate that existing success payload consumers remain compatible.

## Risks and Mitigations
- Risk: help text drift across command groups.
  - Mitigation: add explicit help assertions in smoke tests.
- Risk: over-tightened parsing breaks accepted aliases.
  - Mitigation: alias regression tests for all bounded-domain parsers.
- Risk: output contract drift for existing automation.
  - Mitigation: preserve envelope shape and keep backward-compatible fields when
    adding normalized keys.
