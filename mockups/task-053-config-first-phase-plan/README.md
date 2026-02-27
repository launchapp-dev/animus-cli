# TASK-053 Wireframes: Config-First Workflow Phase Plan Resolution

Concrete wireframes for deterministic phase-plan resolution across workflow run,
planning execute, fallback compatibility, and invalid-config recovery.

## Files
- `wireframes.html`: visual boards for config authoring, workflow run, planning
  execute parity, workflow inspection, fallback handling, invalid-config errors,
  and `320px` narrow-terminal behavior.
- `wireframes.css`: shared visual system for hierarchy, spacing, focus states,
  CLI output readability, and responsive layouts.
- `phase-plan-config-wireframe.tsx`: React-oriented state scaffold for
  config/fallback/error resolution modes and cross-path parity validation.

## Surface Coverage

| UX brief surface | Covered in |
| --- | --- |
| `S1` Pipeline config authoring (`.ao/state/workflow-config.v2.json`) | `wireframes.html` (`S1 Config Authoring`) + `phase-plan-config-wireframe.tsx` (`SCENARIOS.config`) |
| `S2` Workflow run (`ao workflow run`) | `wireframes.html` (`S2 Direct Workflow Run`) + `phase-plan-config-wireframe.tsx` (`WorkflowRunSurfaceWireframe`) |
| `S3` Requirements execution with workflow start | `wireframes.html` (`S3 Planning Execute Parity`) + `phase-plan-config-wireframe.tsx` (`PlanningParityWireframe`) |
| `S4` Workflow inspection (`ao workflow get`, checkpoints) | `wireframes.html` (`S4 Workflow Inspection`) + `phase-plan-config-wireframe.tsx` (`InspectionSurfaceWireframe`) |
| `S5` Misconfiguration error surface | `wireframes.html` (`S5 Fallback and Invalid Config`) + `phase-plan-config-wireframe.tsx` (`ErrorSurfaceWireframe`) |

## State Coverage
- Resolution source states: `config`, `fallback-missing-config`,
  `invalid-config`.
- Execution outcomes: `started`, `started-with-fallback`, `failed`.
- Parity states: `match`, `mismatch`, `not-applicable`.
- Terminal width states: `>=100 cols`, `80-99 cols`, `<80 cols` (modeled with
  explicit one-per-line phase rendering at mobile board).

## Acceptance Criteria Traceability

| AC | Wireframe trace |
| --- | --- |
| `AC-01` | `S2 Direct Workflow Run` source indicator and ordered config phase list |
| `AC-02` | `S3 Planning Execute Parity` side-by-side run/execute phase-order parity board |
| `AC-03` | `S5 Fallback and Invalid Config` fallback panel with explicit missing-config reason |
| `AC-04` | `S5 Fallback and Invalid Config` fast-fail invalid config panel and JSON envelope |
| `AC-05` | `S1 Config Authoring` includes config-only phase insertion (`accessibility-audit`) reflected in `S2`/`S3` |
| `AC-06` | `phase-plan-config-wireframe.tsx` deterministic helper logic for stable phase ordering and source-specific output structure |

## Accessibility and Responsive Intent
- Source, fallback, and failure are communicated with explicit text labels; no
  color-only cues.
- Command and output blocks are ASCII-safe and copy-paste oriented.
- Error surface includes exact config path and actionable rerun guidance.
- Focus-visible styles are present for all interactive controls.
- Mobile board demonstrates `<80 cols` behavior with numbered one-per-line
  phases and no page-level horizontal overflow.
