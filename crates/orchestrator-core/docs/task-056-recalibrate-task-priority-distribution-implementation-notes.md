# TASK-056 Implementation Notes: Priority Budget and Rebalance Flow

## Purpose
Translate TASK-056 requirements into a low-risk implementation plan that
rebalances existing task priorities and introduces an explicit budget signal for
future drift detection.

## Chosen Strategy
Implement a two-stage flow:
1. **Evaluate** current priority distribution against policy.
2. **Plan/apply** deterministic rebalance changes through task APIs.

Key decisions:
- Keep existing priority model (`critical|high|medium|low`).
- Use a `20%` default high-priority budget over active tasks.
- Reserve `critical` for blocked active tasks only.
- Keep ordered backlog positioning out of scope for this task.

## Non-Negotiable Constraints
- No direct edits to `/.ao/*.json`.
- Deterministic output for identical task snapshots.
- Existing `task prioritized` ordering semantics remain unchanged (priority rank
  + timestamp + id).
- Output contract stays under `ao.cli.v1`.

## Proposed Change Surface

### 1) Core Policy Evaluation and Rebalance Helpers
- File: `crates/orchestrator-core/src/services/task_shared.rs`
- Add pure helpers for:
  - high-budget computation,
  - compliance report generation,
  - deterministic high-candidate ranking,
  - rebalance planning (`Vec<task_id, from, to>`),
  - optional apply helper over in-memory task list.
- Keep helpers side-effect free where possible for easier testing.

### 2) Shared Types for Policy Reporting
- File: `crates/orchestrator-core/src/types.rs`
- Add serializable DTOs as needed, for example:
  - `TaskPriorityPolicyReport`,
  - `TaskPriorityRebalanceChange`,
  - `TaskPriorityRebalancePlan`.
- Keep field naming explicit and JSON-friendly for CLI output.

### 3) CLI Task-Control Command
- File: `crates/orchestrator-cli/src/cli_types/task_control_types.rs`
- Add a new task-control subcommand for rebalance with:
  - budget input (`--high-budget-percent`, default `20`),
  - dry-run/apply control,
  - confirmation token for apply,
  - optional explicit high/low override task-id lists.

- File: `crates/orchestrator-cli/src/services/operations/ops_task_control.rs`
- Implement handler flow:
  1. load tasks,
  2. compute policy report + rebalance plan,
  3. return dry-run payload or apply with confirmation,
  4. emit before/after counts and changed task list.

### 4) Task Stats Policy Visibility
- File: `crates/orchestrator-cli/src/services/runtime/runtime_project_task/task.rs`
- Extend `TaskCommand::Stats` output with policy compliance metadata.
- Preserve existing fields to avoid contract regressions.

### 5) Tests
- File: `crates/orchestrator-core/src/services/tests.rs`
  - add policy evaluator math tests,
  - add deterministic tie-break tests,
  - add blocked-to-critical mapping assertions.
- File: `crates/orchestrator-cli/tests/cli_e2e.rs`
  - add dry-run rebalance contract test,
  - add apply rebalance confirmation and mutation test.
- File: `crates/orchestrator-cli/tests/cli_json_contract.rs`
  - assert new `task stats` policy fields exist and are typed correctly.

## Recommended Sequencing
1. Add core policy structs/helpers and unit tests first.
2. Wire task-control CLI command to compute dry-run plan.
3. Add apply mode with destructive confirmation.
4. Extend `task stats` with policy report.
5. Add/adjust e2e + JSON contract tests.
6. Run targeted test suites and fix regressions.

## Risks and Mitigations
- Risk: budget rounding ambiguity (`floor` vs `ceil`) causes test drift.
  - Mitigation: codify one rule in requirements-driven tests and expose it in
    command output.
- Risk: repeated runs produce unstable change plans.
  - Mitigation: enforce explicit tie-break chain ending in task id.
- Risk: operator surprise from bulk mutation.
  - Mitigation: default dry-run path, explicit confirmation for apply, and
    clear before/after report.
- Risk: stats contract regression.
  - Mitigation: additive fields only; update JSON contract tests.

## Validation Targets
- `cargo test -p orchestrator-core services::tests::task_service_supports_priority_checklists_and_dependencies -- --nocapture`
- `cargo test -p orchestrator-core services::tests -- --nocapture`
- `cargo test -p orchestrator-cli --test cli_json_contract -- --nocapture`
- `cargo test -p orchestrator-cli --test cli_e2e -- --nocapture`
