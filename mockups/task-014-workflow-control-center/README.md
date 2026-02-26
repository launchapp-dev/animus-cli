# TASK-014 Wireframes: Task and Workflow Control Center

Concrete wireframes for a queue-first operations surface across task and
workflow routes, including deterministic state transitions, ordered phase
timeline rendering, and high-impact gating UX.

## Files
- `wireframes.html`: desktop and mobile (`320px`) boards with queue controls,
  workflow actions, timeline ordering, gating, and diagnostics feedback states.
- `wireframes.css`: visual system for hierarchy, spacing, focus visibility,
  responsive stacking, and overflow-safe rendering for long IDs.
- `workflow-control-center-wireframe.tsx`: React-oriented scaffold for queue
  sorting/filtering, status transition guardrails, workflow control actions,
  timeline composition order, and fail-closed confirmation gates.

## Route Coverage

| Route | Covered in |
| --- | --- |
| `/tasks` | `wireframes.html` (`Desktop Command Floor`) + `workflow-control-center-wireframe.tsx` (`TaskQueueCard`) |
| `/tasks/:taskId` | `wireframes.html` (`Queue Transition and Conflict Handling`) + `workflow-control-center-wireframe.tsx` (`TaskQueueCard`) |
| `/workflows` | `wireframes.html` (`Desktop Command Floor`) + `workflow-control-center-wireframe.tsx` (`WorkflowActionsPanel`) |
| `/workflows/:workflowId` | `wireframes.html` (`Desktop Command Floor`) + `workflow-control-center-wireframe.tsx` (`WorkflowActionsPanel`, `PhaseTimelinePanel`) |
| `/workflows/:workflowId/checkpoints/:checkpoint` | `wireframes.html` (`Phase Timeline and Decisions`) + `workflow-control-center-wireframe.tsx` (`PhaseTimelinePanel`) |

## State Coverage
- Queue state: `loading`, `ready`, `empty`, `filtered-empty`, `mutation-pending`,
  `mutation-error`.
- Workflow state: `idle`, `running`, `paused`, `cancelled`, `completed`.
- Gating state: `allowed`, `confirm-required`, `fail-closed`, `submitting`.
- Timeline state: `checkpointed`, `decision`, `approved`, `blocked`, `empty`.
- Telemetry state: `success`, `idempotent-retry`, `conflict`.

## Rule-Based Availability Modeled

| Operation | Enabled when | Disabled rationale |
| --- | --- | --- |
| Task `ready -> in-progress` | task is `ready` and dependency tasks are done | `blocked by unresolved dependency` |
| Task `in-progress -> done` | checklist is complete | `finish checklist before moving to done` |
| Task `in-progress -> cancelled` | gate metadata is present and typed phrase is valid | `confirmation phrase mismatch` |
| Workflow `run` | workflow lifecycle is `idle` and phase gate is `approved` | `gate approval missing` |
| Workflow `resume` | workflow lifecycle is `paused` | `workflow is not paused` |
| Workflow `pause` | workflow lifecycle is `running` | `workflow is not running` |
| Workflow `cancel` | workflow lifecycle is `running` or `paused` and typed phrase is valid | `confirmation phrase mismatch` |

## Mockup Review Updates
- Corrected route references to existing app routes (`/tasks`, `/workflows`,
  and workflow checkpoint detail path).
- Corrected queue ordering contract to `priority -> updated_at -> task ID`.
- Corrected timeline contract to stable ascending order:
  checkpoint order -> timestamp -> stable key.
- Corrected sample mutation endpoint labels to `/api/v1/workflows/:id/*`.
- Added explicit queue filter/search controls and error recovery cues.
- Added high-impact task cancellation gate alongside workflow cancellation gate.
- Added accessibility-aligned alert/live-region examples for failure and action
  feedback.
- Expanded acceptance traceability to all `AC-01` through `AC-10` criteria.

## Acceptance Criteria Traceability

| AC | Wireframe trace |
| --- | --- |
| `AC-01` | Desktop queue board shows summary strip, filter/search controls, queue list, and actionable detail pane |
| `AC-02` | Task transition drawer and scaffolded transition handlers model API-backed status updates and post-action refresh semantics |
| `AC-03` | Pending/idempotent retry behavior is modeled in `.tsx` dispatch paths and telemetry board conflict/retry examples |
| `AC-04` | Workflow action controls include `run`, `pause`, `resume`, and `cancel` with success/error feedback and correlation metadata |
| `AC-05` | Timeline board and `.tsx` sort helper model deterministic checkpoint/timestamp/key ordering |
| `AC-06` | Typed confirmation dialogs for workflow cancel and task cancel transitions block dispatch when incomplete |
| `AC-07` | Inline normalized error examples and telemetry entries expose action, code/message context, and correlation IDs |
| `AC-08` | Focus-visible styles, semantic regions, labeled controls, and dialog semantics are reflected in HTML/CSS scaffold |
| `AC-09` | Mobile `320px` board demonstrates stacked controls, wrapped IDs, and no page-level horizontal overflow |
| `AC-10` | Mockups remain scoped to task/workflow surfaces and do not alter shell navigation or `api_only` assumptions |

## Accessibility and Responsive Intent
- Primary sections use landmark-like grouping with heading hierarchy.
- Controls preserve keyboard focus visibility and `44px` minimum touch targets.
- Action failures are modeled with `role="alert"` and outcome updates with
  `aria-live="polite"`.
- Dialog examples include `role="dialog"` and `aria-modal` semantics.
- Mobile board demonstrates stacked layout at `320px` with no page-level
  horizontal scrolling.
