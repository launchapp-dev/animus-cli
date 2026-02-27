# TASK-053 UX Brief: Config-First Workflow Phase Plan Resolution

## Phase
- Workflow phase: `ux-research`
- Workflow ID: `eff3d904-23ba-47f5-915b-aa6d36afe9d6`
- Task: `TASK-053`

## Inputs
- Requirements baseline: `crates/orchestrator-core/docs/task-053-load-workflow-phase-plans-from-config-requirements.md`
- Implementation notes: `crates/orchestrator-core/docs/task-053-load-workflow-phase-plans-from-config-implementation-notes.md`
- Runtime phase-plan call sites:
  - `crates/orchestrator-core/src/services/workflow_impl.rs`
  - `crates/orchestrator-core/src/services/planning_shared.rs`
  - `crates/orchestrator-core/src/workflow/phase_plan.rs`
- Config model and phase catalog source: `crates/orchestrator-core/src/workflow_config.rs`

## UX Objective
Ensure operators can trust that configured pipeline phases are the phases that run.

The user experience for this task must make phase-plan source and behavior
deterministic:
1. If workflow config exists and is valid, execution uses config-defined phase order.
2. If config is missing, fallback is explicit and predictable.
3. If config exists but is invalid, failure is immediate and actionable (no silent fallback).

## Primary Users and Success Signals

| User | Primary job | UX success signal |
| --- | --- | --- |
| AO operator | Run/resume workflows for a task safely | Can predict current/next phase from config without reading source code |
| Project maintainer | Update pipeline phases in config | A config-only phase change appears in newly started workflows |
| Automation engineer | Branch on workflow run outcomes in scripts | Can detect fallback/error states via stable text and `--json` envelopes |

## Key Screens (CLI and Config Surfaces)

| Screen ID | Surface | User goal | Required hierarchy |
| --- | --- | --- | --- |
| S1 | Pipeline config authoring (`.ao/state/workflow-config.v2.json`) | Define/adjust ordered phases per pipeline | pipeline id -> ordered phases -> save |
| S2 | Workflow run (`ao workflow run --task-id ... --pipeline-id ...`) | Start a workflow with expected phase order | request -> resolved pipeline -> resulting phase order |
| S3 | Requirements execution with workflow start (`execute/planning` flow) | Materialize tasks and auto-start workflows with matching pipeline phases | execute summary -> started workflows -> applied pipeline phases |
| S4 | Workflow inspection (`ao workflow get`, checkpoints/decisions surfaces) | Verify active and upcoming phases reflect config | workflow status -> current phase -> full ordered phase list |
| S5 | Misconfiguration error surface (text + `--json`) | Recover from invalid config quickly | failure reason -> config path -> next remediation step |

## Interaction and State Model

| Surface | Trigger | Primary interaction | State transition |
| --- | --- | --- | --- |
| S1 -> S2 | Maintainer updates pipeline phases | Save config and run workflow for target pipeline | `config-saved` -> `workflow-started` |
| S2 -> S4 | Operator starts workflow | Inspect workflow details/checkpoints | `workflow-started` -> `phase-verified` |
| S3 -> S4 | Operator runs requirement execution with auto-start | Confirm workflows start with selected/default pipeline | `requirements-executed` -> `workflows-verified` |
| S2/S3 -> S5 | Config parse/validation failure | Read actionable error and rerun after fix | `run-failed` -> `config-fixed` -> `rerun-success` |
| S2/S3 fallback path | Config file is missing | Continue with built-in plan and explicit source indicator | `config-missing` -> `fallback-applied` |

## Critical User Flows

### Flow A: Config-Driven Phase Change Takes Effect
1. Maintainer adds or reorders phases in `.ao/state/workflow-config.v2.json`.
2. Operator runs `ao workflow run` for the affected pipeline.
3. Workflow starts with phase list matching config order.
4. Operator confirms phase order via workflow inspection surface.

### Flow B: Planning/Execute Path Uses Same Phase Source
1. Operator runs requirement execution with `start_workflows=true`.
2. System creates/reuses tasks and starts workflows.
3. Started workflows use config-driven phases for selected/default pipeline.
4. Operator verifies no phase-order drift between direct workflow run and planning execute path.

### Flow C: Missing Config Compatibility Fallback
1. Operator runs workflow-related command in a repo without workflow config file.
2. System applies built-in phase plan.
3. Output still remains deterministic and indicates fallback source.
4. Operator can continue work without manual recovery steps.

### Flow D: Invalid Config Fast-Fail Recovery
1. Config file exists but has invalid schema/version/content.
2. Workflow start path fails immediately.
3. Error identifies config location and why validation failed.
4. Operator fixes config and reruns command.

## Content Hierarchy and Spacing Guidance
For workflow-start and planning-execute outputs touching phase plans:
1. Outcome line first (`started`, `failed`, `fallback-used`).
2. Pipeline context second (requested id and resolved source).
3. Ordered phase list third (in execution order).
4. Next-step guidance last (inspect workflow, fix config, rerun).

Spacing rules:
- Keep one blank line between outcome block, phase list block, and next-step block.
- Keep phase identifiers one per token with stable ordering.
- Avoid burying source/fallback context below verbose logs.

## Responsive Terminal Behavior
- `>= 100 cols`: allow compact phase list on one line when short.
- `80-99 cols`: wrap at separators while preserving phase order.
- `< 80 cols`: render phases one-per-line with numeric order to avoid horizontal scroll.

## Accessibility Constraints (Non-Negotiable)
1. Do not rely on color to communicate phase source, fallback, or failure.
2. Preserve deterministic token order for human scanning and assistive parsing.
3. Keep command/output text ASCII-safe and copy-paste safe.
4. Use explicit file paths and phase IDs in error messages (no ambiguous pronouns).
5. Provide remediation text that can be executed directly (`fix config`, then rerun).
6. Preserve `ao.cli.v1` JSON envelope shape for automation and screen-reader tooling.
7. Keep keyboard-only operation complete; no interactive prompt dependency for recovery.

## Risks and Mitigations

| Risk | UX impact | Mitigation |
| --- | --- | --- |
| Silent fallback when config is present | Operator distrusts pipeline behavior | Enforce explicit invalid-config error; reserve fallback for missing config only |
| Different phase sources between run paths | Inconsistent execution mental model | Use one resolver contract in workflow + planning execution paths |
| Long phase lists become unreadable in narrow terminals | Missed phase order and wrong expectations | Width-aware wrapping rules with stable ordering |
| Vague config errors slow recovery | High rerun friction and support overhead | Include exact config path, validation reason, and immediate next step |

## Requirements Traceability

| Requirement | UX coverage |
| --- | --- |
| `FR-01` Config-first resolution | S1, S2, S3, Flow A, Flow B |
| `FR-02` Missing-config fallback | S2/S3 fallback path, Flow C |
| `FR-03` Invalid-config explicit error | S5, Flow D |
| `FR-04` Cross-path consistency | S2 vs S3 parity, Flow B |
| `FR-05` Pipeline extensibility | Flow A, S4 verification |
| `FR-06` Regression coverage expectations | S2-S5 deterministic output contracts |

## Implementation Handoff Checklist
- Keep phase-plan source explicit in workflow-related outputs (config vs fallback).
- Ensure direct workflow runs and planning-triggered runs present consistent phase ordering semantics.
- Keep error messages concise, path-specific, and actionable for immediate rerun.
- Preserve deterministic output structure in both plain text and `--json` flows.
- Validate UX-critical behaviors with tests for config-driven order, missing-config fallback, and invalid-config failure.
