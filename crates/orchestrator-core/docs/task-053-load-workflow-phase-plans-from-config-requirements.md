# TASK-053 Requirements: Load Workflow Phase Plans from Config Instead of Hardcoding

## Phase
- Workflow phase: `requirements`
- Workflow ID: `eff3d904-23ba-47f5-915b-aa6d36afe9d6`
- Task: `TASK-053`

## Objective
Make workflow phase planning config-first so pipeline phase order is sourced from
`workflow-config.v2.json` at runtime. Keep hardcoded phase plans only as a
fallback when config is missing.

## Current Baseline Audit

| Surface | Current location | Current state | Gap |
| --- | --- | --- | --- |
| Hardcoded phase plans | `crates/orchestrator-core/src/workflow/phase_plan.rs` | `standard` and `ui-ux-standard` phase arrays are duplicated in code | adding/changing a phase requires code change |
| Config phase plans | `crates/orchestrator-core/src/workflow_config.rs` (`WorkflowConfig.pipelines`, `resolve_pipeline_phase_plan`) | config model already carries ordered pipeline phases | config source is not used consistently across all phase-plan call sites |
| Workflow service phase-plan resolution | `crates/orchestrator-core/src/services/workflow_impl.rs` (`resolve_phase_plan`) | uses config for `FileServiceHub` runs, then falls back to hardcoded plans | hardcoded fallback occurs even when config exists but lookup misses |
| Requirements execution workflow bootstrap | `crates/orchestrator-core/src/services/planning_shared.rs` (`execute_requirements_and_record`) | workflow starts always use `phase_plan_for_pipeline_id` hardcoded list | custom pipeline phases in config are ignored in this path |
| Default executor behavior | `crates/orchestrator-core/src/workflow/lifecycle_executor.rs` | default executor starts with hardcoded standard plan | no config-aware default path when project root is known |

## Problem Statement
Phase plan definitions currently exist in two places:
- hardcoded arrays in `workflow/phase_plan.rs`
- dynamic pipeline lists in `workflow-config.v2.json`

Because runtime code still depends on hardcoded arrays in key paths, pipeline
phase changes in config are not consistently honored. This creates drift and
forces code changes for what should be config-only updates.

## Decision for Implementation
Adopt one config-first phase-plan resolution contract:
- When project root is available and `workflow-config.v2.json` exists and is
  valid, phase plans must come from workflow config.
- Hardcoded phase plans remain available only for fallback when config is
  missing (or when no project root context exists, such as pure in-memory use).
- If config exists but is invalid, return an actionable error instead of
  silently falling back to hardcoded plans.

## Scope
In scope for implementation after this requirements phase:
- Introduce/standardize a config-first phase-plan resolver in
  `orchestrator-core`.
- Update workflow run/resume control flow in `workflow_impl` to use the unified
  resolver behavior.
- Update requirements execution workflow bootstrap in `planning_shared` to use
  config-driven plans when running under `FileServiceHub`.
- Keep hardcoded plans as explicit fallback behavior only when config cannot be
  loaded because file is missing or no project-root context is available.
- Add/adjust tests to prove config-driven plans are used in both workflow and
  planning execution paths.

Out of scope:
- Workflow config schema version changes.
- Agent runtime phase schema changes.
- State-machine definition redesign.
- Manual edits to `/.ao/*.json`.

## Constraints
- Preserve deterministic behavior for in-memory service flows that do not have a
  project root.
- Preserve accepted pipeline identifiers and current case-insensitive lookup
  behavior.
- Avoid broad refactors unrelated to phase-plan resolution.
- Keep fallback semantics explicit and narrow:
  - allowed: config file missing / no root context
  - not allowed: using hardcoded plan when config exists but is invalid.

## Functional Requirements

### FR-01: Config-First Resolution
- Phase-plan resolution must read `workflow-config.v2.json` pipelines when a
  project root is available and config loads successfully.

### FR-02: Missing-Config Fallback
- If workflow config is missing, phase-plan resolution must fall back to
  built-in hardcoded plans for compatibility.

### FR-03: Invalid-Config Erroring
- If workflow config exists but cannot be parsed/validated, resolution must fail
  with an actionable error and must not silently use hardcoded plans.

### FR-04: Cross-Path Consistency
- Both `WorkflowServiceApi` flow (`workflow_impl`) and
  `PlanningServiceApi::execute_requirements` flow (`planning_shared`) must use
  the same resolution contract.

### FR-05: Pipeline Plan Extensibility
- Adding a phase to a pipeline in workflow config must affect newly started
  workflows without requiring edits to hardcoded phase arrays.

### FR-06: Regression Coverage
- Tests must verify:
  - config-driven custom pipeline phases are honored,
  - missing-config fallback still works,
  - invalid-config behavior is explicit and non-silent.

## Acceptance Criteria
- `AC-01`: Starting a workflow with a valid project workflow config uses phase
  order from `WorkflowConfig.pipelines`.
- `AC-02`: Requirements execution path that starts workflows also uses config
  phase order for the selected pipeline.
- `AC-03`: Missing workflow config triggers hardcoded fallback behavior (for
  compatibility).
- `AC-04`: Invalid existing workflow config does not silently fall back and
  returns an actionable error.
- `AC-05`: Adding a new phase to a configured pipeline (test fixture) changes
  the resulting workflow phase list without code-level phase array edits.
- `AC-06`: `cargo test -p orchestrator-core` remains green for touched areas.

## Testable Acceptance Checklist
- `T-01`: Workflow service test proving custom pipeline phases from config are
  used.
- `T-02`: Planning execute test proving started workflows use config pipeline
  phases.
- `T-03`: Resolver test for missing-config fallback path.
- `T-04`: Resolver test for invalid-config explicit error path.
- `T-05`: Targeted `orchestrator-core` test run for workflow/planning paths.

## Verification Matrix
| Requirement area | Verification method |
| --- | --- |
| Config-first behavior | workflow + planning tests with custom config pipeline |
| Missing-config compatibility | resolver fallback test on config-absent temp project |
| Misconfiguration safety | invalid-config test asserting actionable error |
| No regression | targeted `orchestrator-core` test runs |

## Implementation Notes (Input to Next Phase)
Primary expected change targets:
- `crates/orchestrator-core/src/workflow/phase_plan.rs`
  - keep fallback plans; add or rework resolver entrypoint to prefer config.
- `crates/orchestrator-core/src/services/workflow_impl.rs`
  - route all phase-plan lookup through the config-first resolver contract.
- `crates/orchestrator-core/src/services/planning_shared.rs`
  - stop directly bootstrapping workflows from hardcoded phase plans in
    file-backed execution path.
- `crates/orchestrator-core/src/services/planning_impl.rs`
  - thread project-root context into planning shared execution where needed.
- `crates/orchestrator-core/src/services/tests.rs` and/or phase-plan unit tests
  - add regression coverage for config-first + fallback/error semantics.

## Deterministic Deliverables for Implementation Phase
- Unified config-first phase-plan resolver behavior.
- Hardcoded plans retained only for missing-config/no-root fallback.
- Workflow and planning runtime paths aligned to the same resolution contract.
- Tests proving config-driven extensibility and guarded fallback behavior.
