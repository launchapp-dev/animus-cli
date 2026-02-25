# TASK-009 Requirements: Harden Confirmation and Dry-Run Behavior for Destructive Commands

## Phase
- Workflow phase: `requirements`
- Workflow ID: `4eb9b944-c4d0-4420-98f5-8e33e2fb28d3`
- Task: `TASK-009`

## Objective
Define a consistent, repository-safe contract for destructive `git`, `task`,
and `workflow` operations so operators always have:
- a deterministic preview (`--dry-run`) before mutation
- an explicit confirmation gate before execution

## Existing Baseline Audit

| Command surface | Current handler path | Confirmation today | Dry-run today | Gap |
| --- | --- | --- | --- | --- |
| `ao git push --force` | `crates/orchestrator-cli/src/services/operations/ops_git/repo.rs` | `--confirmation-id` required when `--force` | none | no preview contract |
| `ao git worktree remove --force` | `crates/orchestrator-cli/src/services/operations/ops_git/worktree.rs` | `--confirmation-id` required when `--force` | none | no preview contract |
| `ao git worktree push --force` | `crates/orchestrator-cli/src/services/operations/ops_git/worktree.rs` | `--confirmation-id` required when `--force` | none | no preview contract |
| `ao task delete --id <TASK>` | `crates/orchestrator-cli/src/services/runtime/runtime_project_task/task.rs` | none | none | no confirmation, no preview |
| `ao task-control cancel --task-id <TASK>` | `crates/orchestrator-cli/src/services/operations/ops_task_control.rs` | none | none | no confirmation, no preview |
| `ao workflow cancel --id <WORKFLOW>` | `crates/orchestrator-cli/src/services/operations/ops_workflow.rs` | none | none | no confirmation, no preview |
| `ao workflow phases remove --phase <PHASE>` | `crates/orchestrator-cli/src/services/operations/ops_workflow.rs` | none | none | no confirmation, no preview |

Additional drift to resolve:
- `crates/orchestrator-cli/src/services/operations/ops_git/confirm.rs` recognizes
  operation types (`remove_repo`, `hard_reset`, `clean_untracked`) that are not
  enforced by active command handlers in this crate.

## Scope
In scope for implementation after this phase:
- Add deterministic `--dry-run` preview behavior for the destructive commands
  listed above.
- Require explicit confirmation before live execution for all listed
  destructive commands.
- Align output shape for preview and confirmation failures in both JSON and
  non-JSON modes.
- Keep command behavior repository-safe and deterministic.

Out of scope for this task:
- Adding new destructive command families outside the audited surfaces.
- Interactive TTY prompts.
- Redesigning workflow/task domain semantics unrelated to safety gating.
- Manual edits to `.ao` JSON state files.

## Constraints
- Preserve existing command names and non-destructive behavior.
- Preserve `ao.cli.v1` envelope behavior when `--json` is enabled.
- Keep exit code mapping unchanged (`2/3/4/5/1`) and map confirmation failures
  to invalid input semantics.
- `--dry-run` must be side-effect free:
  - no git mutations
  - no workflow/task state writes
  - no confirmation outcome writes
- Confirmation material must be bound to operation intent (operation type and
  target identity) to prevent token reuse across unrelated operations.

## Unified Safety Contract
For each in-scope destructive operation:
1. Operator can invoke `--dry-run` to receive a deterministic preview.
2. Live execution requires explicit confirmation artifact (`confirmation-id` or
   equivalent shared confirmation gate).
3. Missing confirmation returns a deterministic failure that includes actionable
   remediation.

### Preview Contract (`--dry-run`)
Preview output must include these fields (names may vary slightly in non-JSON
mode, but content must match):
- `operation`: normalized operation key (for example `task_delete`).
- `target`: command-specific identifiers (task ID, workflow ID, repo/branch,
  worktree name, phase ID).
- `destructive`: `true`.
- `dry_run`: `true`.
- `requires_confirmation`: `true`.
- `planned_effects`: stable ordered list of expected side effects.
- `next_step`: concrete command guidance for executing with confirmation.

### Confirmation Failure Contract
When live execution is attempted without required confirmation, return:
- machine signal containing `CONFIRMATION_REQUIRED`
- clear operator guidance on how to request/approve/provide confirmation
- no side effects

## Acceptance Criteria
- `AC-01`: Every in-scope destructive command accepts `--dry-run`.
- `AC-02`: `--dry-run` on each in-scope command returns deterministic preview
  metadata and performs zero state mutation.
- `AC-03`: Every live in-scope destructive command rejects missing confirmation
  with `CONFIRMATION_REQUIRED`.
- `AC-04`: Confirmation artifacts are operation-bound and cannot be reused for a
  different destructive target.
- `AC-05`: Existing git force flows remain functional with current confirmation
  lifecycle (request/respond/outcome) after alignment.
- `AC-06`: Task and workflow destructive flows gain the same confirmation
  guarantees as git force flows.
- `AC-07`: Non-destructive operations in `git`, `task`, and `workflow` remain
  behaviorally unchanged.
- `AC-08`: JSON-mode responses for previews and confirmation failures are
  consistent across all in-scope command groups.

## Verification Matrix

| Requirement | Verification method |
| --- | --- |
| `AC-01`, `AC-02` | CLI integration tests invoking each in-scope command with `--dry-run` and asserting no state/git mutation |
| `AC-03`, `AC-04` | Negative tests for missing/mismatched confirmation IDs and token reuse |
| `AC-05` | Regression tests for existing `git` force commands (`push`, `worktree remove`, `worktree push`) |
| `AC-06` | New tests for `task delete`, `task-control cancel`, `workflow cancel`, and `workflow phases remove` confirmation enforcement |
| `AC-07` | Smoke/regression tests for unaffected non-destructive command paths |
| `AC-08` | Snapshot/assertion tests for JSON response keys across command groups |

## Deterministic Deliverables for Next Phase
- CLI arg updates for in-scope destructive commands.
- Shared destructive preview/confirmation helper(s) to reduce drift between
  command groups.
- Tests covering dry-run safety and confirmation enforcement across git/task/workflow.
