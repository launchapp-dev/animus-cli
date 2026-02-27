# TASK-056 Requirements: Re-calibrate Task Priority Distribution

## Phase
- Workflow phase: `requirements`
- Workflow ID: `df2161dd-5d6b-46f2-ad2a-83b6fd0979fd`
- Task: `TASK-056`

## Objective
Restore task-priority signal quality so priority actively communicates execution
urgency:
- `critical` reserved for blockers only,
- `high` limited to current-sprint essentials,
- `medium` as default planned-work priority,
- `low` for explicit nice-to-have work.

## Current Baseline Audit
Snapshot date: `2026-02-27`.

Observed baseline mismatch:
- Task description baseline: `25/45` tasks marked `high` (`55.56%`).
- Current canonical project root (`/Users/samishukri/ao-cli`) baseline from
  `ao task stats --json`: `28/57` tasks marked `high` (`49.12%`).
- Active tasks (`in-progress|blocked`): `11` total, `4` high (`36.36%`), still
  above a `20%` high-priority budget.

| Surface | Current location | Current behavior | Gap |
| --- | --- | --- | --- |
| Priority ordering | `crates/orchestrator-core/src/services/task_shared.rs` (`sort_tasks_by_priority`) | strict rank `critical > high > medium > low`, then `updated_at`, then `id` | no policy defining when tasks should be promoted/demoted |
| Task-level priority mutation | `crates/orchestrator-cli/src/services/operations/ops_task_control.rs` (`SetPriority`) | can only set one task at a time | no deterministic project-wide recalibration flow |
| Runtime scheduling input | `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_project_tick.rs` (`run_ready_task_workflows_for_project`) | scheduler consumes prioritized task list directly | too many high-priority tasks flatten queue meaning |
| Priority observability | `crates/orchestrator-core/src/services/task_shared.rs` (`build_task_statistics`) + `task stats` handler | reports counts only | no policy compliance/budget signal |
| Upstream requirement quality checks | `crates/orchestrator-cli/src/services/operations/ops_planning/requirements_runtime.rs` | warns when requirement MoSCoW distribution is imbalanced | no equivalent task-priority recalibration or budget enforcement |

## Problem Statement
Task priority values are currently over-concentrated in `high`, reducing
triage quality and weakening queue ordering. Existing operations require manual
per-task edits and provide no policy-aware bulk rebalance path.

## Decision for Implementation
Adopt a **priority-budget policy with deterministic rebalance tooling**.

Decision specifics:
- Keep existing priority enum (`critical|high|medium|low`).
- Add a deterministic bulk rebalance operation with dry-run and apply modes.
- Enforce a configurable high-priority budget (default `20%`) against active
  tasks.
- Reserve `critical` for blocked tasks only.
- Defer backlog-position model changes (ordered backlog/rank field) to a
  follow-up task.

## Scope
In scope for implementation after this requirements phase:
- Add a bulk priority recalibration command under task-control operations.
- Support dry-run planning output and explicit apply mode with confirmation.
- Compute a deterministic policy report (before/after counts and budget
  compliance).
- Reclassify priorities with deterministic rules and optional explicit
  overrides.
- Add regression tests for policy evaluation and CLI behavior.

Out of scope:
- Introducing a new `backlog_rank` field or replacing priority with ordered
  backlog position.
- Freeform semantic/NLP classification of task importance.
- Manual edits to `/.ao/*.json`.
- Changing priority enum values or parse contracts.

## Constraints
- Changes must be deterministic and replayable.
- Mutations must go through AO task APIs (`tasks.list`, `tasks.replace`) rather
  than direct file writes.
- Existing `task prioritized` ordering semantics must remain unchanged except
  for updated priority values.
- JSON envelope contract (`ao.cli.v1`) must remain stable.

## Functional Requirements

### FR-01: Priority Policy Evaluation
- Provide a reusable evaluator that computes:
  - total/active counts by priority,
  - configured high-budget percentage,
  - computed high-budget limit for active tasks,
  - budget-compliance verdict and overflow count.

Active tasks for budget purposes are tasks whose status is not terminal
(`done|cancelled`).

### FR-02: Deterministic Rebalance Rules
- Rebalance classification must follow this deterministic order:
  1. `critical`: active tasks in `blocked` status.
  2. `high`: active, non-blocked tasks selected up to high-budget cap.
  3. `low`: tasks explicitly marked as nice-to-have via command override and
     tasks already `low` when not promoted by rules 1-2.
  4. `medium`: all remaining tasks.

### FR-03: Deterministic High Candidate Ranking
- High-candidate ranking must be stable and deterministic:
  1. explicit essential-task overrides (if provided),
  2. status rank (`in-progress` before `ready/backlog`),
  3. earliest deadline first (unset deadline last),
  4. most recently updated first,
  5. lexicographic task id.

### FR-04: Bulk CLI Operation
- Add a task-control command to:
  - generate a dry-run rebalance plan (no mutation),
  - apply the rebalance plan when explicitly confirmed.
- Output must include:
  - policy inputs,
  - before/after priority distribution,
  - sorted list of changed task ids with from/to priority.

### FR-05: Safe Apply Semantics
- Apply mode must update only tasks whose priority changes.
- Apply mode must preserve unrelated fields.
- Metadata updates (`updated_at`, `updated_by`, version bump) must flow through
  existing task replace/update semantics.

### FR-06: Budget Visibility in Stats
- `task stats` output must expose high-budget compliance information so
  operators can detect drift without running rebalance.

### FR-07: Regression Coverage
- Tests must verify policy math, deterministic ordering, dry-run behavior, and
  apply semantics.

## Acceptance Criteria
- `AC-01`: On the current project baseline (`57` tasks, `11` active,
  `4` active-high), policy evaluation with `20%` budget reports non-compliance.
- `AC-02`: Dry-run rebalance with `20%` budget proposes at most
  `floor(11 * 0.20) = 2` high active tasks (or documented configured rounding
  behavior if implementation chooses ceil/min semantics).
- `AC-03`: All blocked active tasks are assigned `critical` in rebalance
  output.
- `AC-04`: Apply mode changes only tasks listed in dry-run plan and produces a
  deterministic post-change distribution.
- `AC-05`: Re-running dry-run with unchanged input state yields an identical
  change plan.
- `AC-06`: `task stats` includes budget compliance signal in JSON output.
- `AC-07`: Existing `task prioritized` and daemon ready-task scheduling continue
  to function with updated priorities.

## Testable Acceptance Checklist
- `T-01`: Add unit tests for policy evaluator math and budget overflow
  detection.
- `T-02`: Add unit tests for deterministic ranking/tie-break behavior.
- `T-03`: Add CLI/e2e test for dry-run rebalance output contract.
- `T-04`: Add CLI/e2e test for apply rebalance with explicit confirmation.
- `T-05`: Add/extend `task stats` JSON contract tests for policy fields.
- `T-06`: Run targeted test suites for touched crates.

## Verification Matrix
| Requirement area | Verification method |
| --- | --- |
| Policy math and compliance | unit tests over synthetic task sets |
| Deterministic planning | repeat dry-run assertions with identical snapshots |
| Safe mutation path | apply test verifying only expected task updates |
| Operator visibility | `task stats` JSON contract assertions |
| Runtime compatibility | existing task prioritization/scheduler tests |

## Implementation Notes (Input to Next Phase)
Primary expected change targets:
- `crates/orchestrator-core/src/services/task_shared.rs`
  - add policy evaluation + deterministic rebalance helpers.
- `crates/orchestrator-core/src/types.rs`
  - add task-priority-policy result DTOs as needed.
- `crates/orchestrator-cli/src/cli_types/task_control_types.rs`
  - add rebalance command args.
- `crates/orchestrator-cli/src/services/operations/ops_task_control.rs`
  - implement dry-run/apply rebalance handler.
- `crates/orchestrator-cli/src/services/runtime/runtime_project_task/task.rs`
  - expose budget signal in stats output.
- `crates/orchestrator-core/src/services/tests.rs`
  - add policy/determinism tests.
- `crates/orchestrator-cli/tests/{cli_e2e.rs,cli_json_contract.rs}`
  - add command and JSON contract coverage.

## Deterministic Deliverables for Implementation Phase
- Policy-aware rebalance command with deterministic dry-run and apply behavior.
- Enforced interpretation of `critical` as blocker-only.
- High-priority budget visibility and compliance reporting.
- Regression tests covering policy math, deterministic ranking, and mutation
  safety.
