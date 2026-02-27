# TASK-053 Implementation Notes: Config-First Workflow Phase Plans

## Purpose
Translate the requirements decision into a low-risk implementation plan that
eliminates duplicated hardcoded pipeline phase ordering from runtime execution
paths while preserving compatibility when config is missing.

## Chosen Strategy
Centralize phase-plan resolution around workflow config:
- Prefer `workflow-config.v2.json` when project-root context is available.
- Keep existing hardcoded `standard` / `ui-ux-standard` plans as explicit
  fallback only for config-missing/no-root scenarios.
- Use one resolver contract across workflow lifecycle operations and planning
  requirement execution workflow starts.

## Non-Negotiable Constraints
- No direct/manual edits to `/.ao/*.json`.
- No workflow-config schema/version changes in this task.
- Do not broaden scope into state-machine redesign.
- Keep in-memory behavior deterministic without requiring filesystem config.
- Keep fallback path deterministic and explicit.

## Proposed Change Surface

### 1) Phase-Plan Resolver Consolidation
- File: `crates/orchestrator-core/src/workflow/phase_plan.rs`
- Keep hardcoded helpers as fallback source of truth.
- Add config-aware resolver entrypoint(s) with explicit root context, for
  example:
  - `resolve_phase_plan_for_pipeline(project_root: Option<&Path>, pipeline_id: Option<&str>)`
- Behavior contract:
  - config exists + valid: use `resolve_pipeline_phase_plan`.
  - config missing: use hardcoded fallback.
  - config exists + invalid: return error.

### 2) Workflow Service Path Alignment
- File: `crates/orchestrator-core/src/services/workflow_impl.rs`
- Replace local phase-plan branching with the shared resolver contract.
- Ensure `run`, `resume`, `pause`, `cancel`, `complete_current_phase`,
  `fail_current_phase`, and merge-conflict transitions all use consistent
  phase-plan rehydration semantics.

### 3) Planning Requirements Execution Alignment
- Files:
  - `crates/orchestrator-core/src/services/planning_shared.rs`
  - `crates/orchestrator-core/src/services/planning_impl.rs`
- Thread project-root context into `execute_requirements_and_record` so
  file-backed planning runs can resolve phase plans from config.
- Preserve current in-memory call path by passing `None` project-root context,
  which should continue using fallback behavior.

### 4) Public Re-exports and Call-Site Hygiene
- Files:
  - `crates/orchestrator-core/src/workflow.rs`
  - `crates/orchestrator-core/src/lib.rs`
  - `crates/orchestrator-core/src/services.rs` (imports only as needed)
- Keep existing public constants (`STANDARD_PIPELINE_ID`,
  `UI_UX_PIPELINE_ID`) for compatibility.
- Export new resolver only if required by existing crate boundaries.

### 5) Test Coverage Updates
- Files:
  - `crates/orchestrator-core/src/services/tests.rs`
  - `crates/orchestrator-core/src/workflow/phase_plan.rs` (unit tests module)
- Add focused tests for:
  - config-driven custom pipeline phase order in workflow service flow,
  - config-driven phase order in planning execution flow,
  - missing-config fallback behavior,
  - invalid existing config error behavior.

## Sequencing Plan
1. Introduce config-aware resolver in `phase_plan.rs`.
2. Wire `workflow_impl` to resolver and remove duplicate fallback logic there.
3. Pass project-root context through planning execution and use resolver there.
4. Add/adjust tests for config-first, fallback, and invalid-config semantics.
5. Run targeted `orchestrator-core` tests and fix regressions.

## Risks and Mitigations
- Risk: hidden callers rely on silent fallback when config is invalid.
  - Mitigation: keep fallback only for missing config; surface clear error for
    invalid config to prevent silent drift.
- Risk: planning path still bypasses config due incomplete plumbing.
  - Mitigation: add explicit planning execution test with custom pipeline.
- Risk: over-scoping through wide call-site churn.
  - Mitigation: keep resolver API narrow and adjust only direct phase-plan
    consumers.

## Validation Targets
- `cargo test -p orchestrator-core services::tests::file_hub_uses_custom_pipeline_from_workflow_config_v2`
- `cargo test -p orchestrator-core services::tests::planning_service_drafts_and_executes_requirements`
- `cargo test -p orchestrator-core workflow_config::tests`
- `cargo test -p orchestrator-core`
