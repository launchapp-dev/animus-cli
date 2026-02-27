# TASK-051 Requirements: Warn When Creating Tasks Without Linked Requirements

## Phase
- Workflow phase: `requirements`
- Workflow ID: `8ebca88e-570c-433f-92b0-4e4be9b49f00`
- Task: `TASK-051`
- Requirement: unlinked in current task metadata

## Objective
Reduce traceability gaps without blocking automation by adding a non-fatal warning when
`ao task create` creates a non-`chore` task without any linked requirement.

Context from task brief:
- 21 of 45 tasks currently have no linked requirements.
- This task must encourage linking, not enforce hard failure semantics.

## Current Baseline Audit

| Surface | Current location | Current behavior | Gap |
| --- | --- | --- | --- |
| CLI `task create` flags | `crates/orchestrator-cli/src/cli_types/task_types.rs` (`TaskCreateArgs`) | supports `--title`, `--description`, `--task-type`, `--priority`, `--linked-architecture-entity`, `--input-json` | no `--linked-requirement` input path |
| CLI `task create` runtime mapping | `crates/orchestrator-cli/src/services/runtime/runtime_project_task/task.rs` (`TaskCommand::Create`) | builds `TaskCreateInput` with `linked_requirements: Vec::new()` unless provided via `--input-json` | missing warning path for unlinked non-`chore` tasks |
| Input JSON precedence | `crates/orchestrator-cli/src/shared/parsing.rs` (`parse_input_json_or`) | when `--input-json` is provided, payload overrides individual flags | warning logic must evaluate resolved payload, not raw flags |
| Task creation defaults | `crates/orchestrator-core/src/services/task_impl.rs` (`create`) | defaults missing `task_type` to `feature`; persists linked requirements as provided | default `feature` tasks can be created silently with zero linked requirements |
| MCP `ao.task.create` surface | `crates/orchestrator-cli/src/services/operations/ops_mcp.rs` | task-create tool does not expose linked requirement input | automation via MCP cannot proactively include requirement linkage |

## Scope
In scope for implementation after this requirements phase:
- Add `--linked-requirement <REQ_ID>` to `ao task create`.
- Allow repeated `--linked-requirement` flags to populate `linked_requirements`.
- Emit a warning when the resolved create payload is:
  - `task_type != chore` (including defaulted `feature`), and
  - `linked_requirements` is empty.
- Keep task creation successful (warning-only, no hard failure).
- Keep warning behavior deterministic for both `--json` and non-JSON modes.
- Add focused tests for parsing, warning trigger matrix, and non-fatal behavior.
- Add optional MCP parity by allowing `ao.task.create` tool input to pass linked requirement ids to CLI.

Out of scope for this task:
- Converting warning into validation failure.
- Backfilling existing unlinked tasks.
- Auto-linking requirements heuristically.
- Enforcing that linked requirement ids exist at task-create time.
- Manual edits to `/.ao/*.json`.

## Constraints
- Preserve existing `ao.cli.v1` success/error envelope behavior.
- Preserve exit code semantics:
  - warning path must still exit with code `0`.
- Preserve `--input-json` precedence semantics.
- Warning check must run against the final resolved `TaskCreateInput` payload.
- `task_type` omission must be treated as default `feature` for warning logic.
- `chore` tasks are explicitly exempt from warning when unlinked.
- Keep change additive and repository-safe (no unrelated command-surface churn).

## Warning Contract

### Trigger Rule
Emit warning iff all conditions are true:
- command is `ao task create`,
- resolved task type is not `chore`,
- resolved linked requirements list is empty.

### Non-Trigger Cases
Do not emit warning when either condition is true:
- resolved task type is `chore`, or
- at least one linked requirement is present.

### Output Semantics
- Warning is emitted on `stderr`.
- Task create result remains a normal success payload on `stdout` (including under `--json`).
- Warning text is stable and actionable (must mention `--linked-requirement`).

## Functional Requirements

### FR-01: Additive CLI Input Surface
- `ao task create` accepts `--linked-requirement <REQ_ID>`.
- Repeat flag support is allowed for multiple ids.

### FR-02: Resolved Payload Evaluation
- Warning logic evaluates the resolved `TaskCreateInput` after `--input-json` precedence.
- No duplicated warning evaluation paths.

### FR-03: Non-Chore Warning Behavior
- Non-`chore` tasks with no linked requirements emit warning.
- Includes defaulted task type path when `task_type` is omitted.

### FR-04: Chore Exemption
- `task_type == chore` never emits missing-link warning.

### FR-05: Non-Fatal Compatibility
- Warning must not block creation, mutate exit code, or change error classification.

### FR-06: JSON Compatibility
- `--json` success envelope remains parseable and unchanged in core shape.
- Warning emission must not corrupt `stdout` JSON payload.

### FR-07: MCP Parity (If Implemented in This Slice)
- MCP `ao.task.create` input may optionally include linked requirement ids and pass them through to CLI flags.
- MCP callers remain non-blocked when no link is provided (warning-only behavior still applies).

### FR-08: Regression Coverage
- Add tests for:
  - flag parsing,
  - warning trigger/no-trigger matrix,
  - success exit behavior with warning,
  - JSON payload stability under warning path.

## Acceptance Criteria
- `AC-01`: `ao task create` accepts at least one `--linked-requirement` flag value.
- `AC-02`: repeated `--linked-requirement` values populate linked requirements deterministically.
- `AC-03`: creating non-`chore` tasks without linked requirements emits warning.
- `AC-04`: creating `chore` tasks without linked requirements emits no warning.
- `AC-05`: creating any task with linked requirements emits no missing-link warning.
- `AC-06`: warning path still returns success (exit code `0`) and creates the task.
- `AC-07`: `--json` success envelope remains valid JSON on `stdout` while warning is emitted to `stderr`.
- `AC-08`: warning message explicitly references `--linked-requirement`.
- `AC-09`: no unrelated CLI command behavior regresses.

## Testable Acceptance Checklist
- `T-01`: CLI parsing test for `task create --linked-requirement REQ-123`.
- `T-02`: CLI parsing test for repeated `--linked-requirement` values.
- `T-03`: runtime/integration test: non-`chore` unlinked create emits warning + success.
- `T-04`: runtime/integration test: `chore` unlinked create emits no warning.
- `T-05`: runtime/integration test: linked create emits no missing-link warning.
- `T-06`: JSON-mode integration test verifies stdout envelope remains parseable when warning is present.
- `T-07`: (if MCP parity included) MCP tool schema/arg forwarding test covers linked requirement input.

## Verification Matrix

| Requirement area | Verification method |
| --- | --- |
| FR-01, FR-02 | CLI parse tests and runtime create-input mapping tests |
| FR-03, FR-04, FR-05 | integration tests over task create warning trigger matrix |
| FR-06 | JSON contract assertions with warning on `stderr` |
| FR-07 | MCP tool-schema and arg-forwarding tests (if implemented) |
| FR-08 | targeted `cargo test -p orchestrator-cli` for touched modules |

## Implementation Notes (Input to Next Phase)
Primary code surfaces:
- `crates/orchestrator-cli/src/cli_types/task_types.rs`
- `crates/orchestrator-cli/src/services/runtime/runtime_project_task/task.rs`
- `crates/orchestrator-cli/src/cli_types/mod.rs` (parse tests)
- `crates/orchestrator-cli/tests/cli_e2e.rs` or a focused warning-contract integration test file

Optional MCP parity surfaces:
- `crates/orchestrator-cli/src/services/operations/ops_mcp.rs`

Documentation touchpoints:
- `README.md` task creation examples should include a traceable path using `--linked-requirement`.

## Deterministic Deliverables for Implementation Phase
- Additive `task create` linked-requirement flag support.
- Deterministic warning behavior for missing linked requirements on non-`chore` tasks.
- Non-fatal, automation-safe output contract preservation.
- Focused tests proving warning behavior and JSON compatibility.
