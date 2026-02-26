# TASK-002 Implementation Notes: CLI Help and Error Message Polish

## Phase Context
- Workflow phase: `mockup-review`
- Workflow ID: `c280da10-e502-499b-9f57-62e75e158630`
- Task: `TASK-002`

## Purpose
Capture mockup-review outcomes for `TASK-002` and lock deterministic UI/UX
fixtures before implementation and regression test expansion.

## Mockup-Review Outcome
This phase reconciled wireframes and mockup fixtures with scoped CLI behavior
and requirements traceability:
- corrected command-name drift in mockup help boards to match clap subcommands,
- corrected `ao task update --help` mockup options to implemented fields,
- corrected `--input-json` mockup semantics to payload precedence,
- corrected `task-control set-deadline` formatting guidance to RFC3339,
- preserved canonical confirmation and shared dry-run key contracts.

## Non-Negotiable Constraints
- Keep command names and primary flag names backward compatible.
- Preserve `ao.cli.v1` success/error envelope shape.
- Preserve existing exit-code classification semantics.
- Keep destructive operations side-effect free when `--dry-run` is set.
- Do not manually edit `.ao` JSON state files.

## Confirmed Baseline (Before Implementation)
- `cli_types.rs` already contains broad command/argument help coverage and
  bounded-value hints for key surfaces.
- Shared parsing and requirements parsing already emit actionable invalid-value
  messages with accepted values and a help hint.
- `cli_smoke.rs` and `cli_e2e.rs` already include meaningful help and
  confirmation coverage.
- Remaining drift exists in canonical confirmation wording and explicit dry-run
  shared-key regression assertions.

## Mockup Mismatch Resolution Matrix

| Surface | Mismatch found in review | Resolution applied | Acceptance criteria |
| --- | --- | --- | --- |
| Root/group help | Root title and command descriptions drifted from current clap copy | Updated fixtures to match current command names and intent text | `AC-01`, `AC-10` |
| Task group help | Included non-existent command names (`add-dependency`, `assign-owner`) | Replaced with implemented subcommands (`dependency-add`, `dependency-remove`, `assign-agent`) | `AC-01`, `AC-02` |
| Task update command help | Included non-implemented options (`--type`, `--deadline`) | Replaced with implemented options and repeatable link guidance | `AC-02`, `AC-03` |
| `--input-json` guidance | Modeled as path/file input | Corrected to payload semantics (`--input-json <JSON>`) with precedence note | `AC-03`, `AC-10` |
| Group audit details | `task-control set-deadline` format and `git`/`requirements` group maps were inaccurate | Corrected to RFC3339 and real group command topology | `AC-01`, `AC-02` |

## Proposed Change Surface

### P0: Confirmation Contract Alignment
- `crates/orchestrator-cli/src/shared/parsing.rs`
  - keep `CONFIRMATION_REQUIRED` contract for `--confirm` flows stable.
- `crates/orchestrator-cli/src/services/operations/ops_git/store.rs`
  - align git-specific `CONFIRMATION_REQUIRED` wording with canonical ordering
    while preserving `--confirmation-id` semantics and approval workflow.
- `crates/orchestrator-cli/tests/cli_e2e.rs`
  - add explicit token-order assertions for non-git and git confirmation paths.

### P0: Dry-Run Preview Schema Stability
- `crates/orchestrator-cli/src/services/runtime/runtime_project_task/task.rs`
- `crates/orchestrator-cli/src/services/operations/ops_task_control.rs`
- `crates/orchestrator-cli/src/services/operations/ops_workflow.rs`
- `crates/orchestrator-cli/src/services/operations/ops_git/repo.rs`
- `crates/orchestrator-cli/src/services/operations/ops_git/worktree.rs`
  - ensure shared destructive preview keys are always present:
    `operation`, `target`, `action`, `destructive`, `dry_run`,
    `requires_confirmation`, `planned_effects`, `next_step`.
- `crates/orchestrator-cli/tests/cli_e2e.rs`
  - assert shared-key presence across scoped destructive dry-run commands.

### P1: Help Copy Drift Audit
- `crates/orchestrator-cli/src/cli_types.rs`
  - adjust only where wording drift/missing guidance is found in scoped command
    groups.
- `crates/orchestrator-cli/tests/cli_smoke.rs`
  - extend assertions for any newly standardized help wording.

### P1: Validation Message Parity
- `crates/orchestrator-cli/src/shared/parsing.rs`
  - align any remaining bounded-domain parse variants to canonical invalid-value
    formatting.
- `crates/orchestrator-cli/src/services/operations/ops_requirements/state.rs`
  - keep requirement-specific invalid-value errors aligned with shared contract.
- parser unit tests and e2e tests
  - assert canonical token order and alias compatibility.

## Implementation Boundaries
- Scope changes to user-visible help/error strings, destructive dry-run payload
  shape, and deterministic test coverage.
- Do not modify command dispatch flow, persistence formats, or domain-state
  transition logic.
- Do not alter `.ao` data files directly.

## Suggested Message Contract

### Invalid-value contract
Preferred shape:
- `invalid <domain> '<value>'; expected one of: <v1>, <v2>, ...; run the same command with --help`

Requirements:
- deterministic ordering of accepted values,
- stable punctuation for test assertions,
- no environment-dependent text.

### Confirmation-required contract
Preferred shapes:
- Non-git:
  `CONFIRMATION_REQUIRED: rerun '<command>' with --confirm <token>; use --dry-run to preview changes`
- Git:
  `CONFIRMATION_REQUIRED: request and approve a git confirmation for '<operation>' on '<repo>', then rerun with --confirmation-id <id>; use --dry-run to preview changes`

Requirements:
- include exact flag name expected by that command path,
- mention preview path when supported,
- keep canonical token order so snapshots remain stable.

### Dry-run preview contract
Shared top-level keys:
- `operation`
- `target`
- `action`
- `destructive`
- `dry_run`
- `requires_confirmation`
- `planned_effects`
- `next_step`

Allow command-specific companion fields but keep shared keys stable.

## Slice Plan
1. Align confirmation-required wording in shared and git-specific paths.
2. Normalize shared dry-run key contract for scoped destructive operations.
3. Apply minimal help copy fixes only where scoped audit finds drift.
4. Add/extend deterministic assertions in smoke/e2e/unit tests.
5. Run targeted tests and fix only regressions introduced by this task.

## Test Plan

### Existing test files to extend
- `crates/orchestrator-cli/tests/cli_smoke.rs`
  - assert scoped help wording where this task standardizes text.
- `crates/orchestrator-cli/tests/cli_e2e.rs`
  - assert confirmation guidance contract/token order.
  - assert dry-run shared-key contract for task/task-control/workflow/git flows.

### Unit tests to expand
- `crates/orchestrator-cli/src/shared/parsing.rs` tests
  - verify canonical invalid-value wording and accepted-values visibility.
  - verify alias values still parse correctly.
- `crates/orchestrator-cli/src/services/operations/ops_requirements/state.rs` tests
  - verify requirement invalid-value messaging remains contract-compatible.

### Targeted validation commands
- `cargo test -p orchestrator-cli --test cli_smoke`
- `cargo test -p orchestrator-cli --test cli_e2e`
- `cargo test -p orchestrator-cli shared::parsing`
- `cargo test -p orchestrator-cli ops_requirements::state`

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
- Risk: canonical message wording mismatch between handlers.
  - Mitigation: enforce shared templates and assert token order in tests.
