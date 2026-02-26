# TASK-014 UX Brief: Task/Workflow Control Center Interface

## Phase
- Workflow phase: `ux-research`
- Workflow ID: `a72d7b8e-e1e8-4804-b925-355318bca593`
- Task: `TASK-014`

## UX Objective
Design a queue-first control center that lets operators manage task and workflow
execution safely under pressure.

The experience must answer three questions quickly:
1. What should I act on next?
2. What state transition or workflow control is currently safe to run?
3. What happened after I acted (including failures and correlation context)?

## Primary Users and Jobs

| User | Primary jobs | UX success signal |
| --- | --- | --- |
| Operator | Triage queue health and move tasks/workflows forward | Can select target work and run the right control action in <= 3 interactions |
| Delivery lead | Keep status transitions and checkpoints accurate | Can confirm task/workflow state changes are deterministic and auditable |
| On-call engineer | Recover quickly from failed actions | Can see failure reason and correlation metadata without opening devtools |

## UX Principles for This Phase
1. Queue clarity first: put priority, status, and next actions in scan order.
2. Deterministic behavior: stable sort, explicit pending states, no duplicate submits.
3. Risk-proportional gating: high-impact actions require deliberate confirmation.
4. Progressive detail: summary first, timeline/checkpoint detail on demand.
5. Keyboard and mobile parity: core flows remain operable at `320px` without horizontal scroll.

## Information Architecture

### In-Scope Routes
- `/tasks`
- `/tasks/:taskId`
- `/workflows`
- `/workflows/:workflowId`
- `/workflows/:workflowId/checkpoints/:checkpoint`

### Task Control Center Structure (`/tasks`)
1. Page header with queue status summary (`stats`, active filters, search).
2. Queue list region (deterministic ordering and selection state).
3. Task detail/action region (status transitions + metadata).
4. Inline feedback and diagnostics linkage for failed actions.

### Workflow Control Center Structure (`/workflows` + `/workflows/:workflowId`)
1. Workflow list with status and quick actions.
2. Workflow action controls (`run`, `pause`, `resume`, `cancel`) with pending locks.
3. Phase/checkpoint timeline ordered by checkpoint -> timestamp -> stable key.
4. Checkpoint detail handoff and diagnostics context.

## Key Screens and Interaction Contracts

| Screen | Goal | Primary interactions | Required states |
| --- | --- | --- | --- |
| `/tasks` | Operate task queue from one surface | Filter/search queue, select task, apply status transition, open detail route | loading, empty, filtered-empty, ready, transition-pending, transition-failure |
| `/tasks/:taskId` | Deep-link task control and context | Review task details, run transition, manage checklist/dependencies as exposed | loading, ready, not-found, mutation-pending, mutation-success, mutation-error |
| `/workflows` | Choose workflow to run or control | Scan workflows, trigger run, navigate to workflow detail | loading, empty, ready, run-pending, run-error |
| `/workflows/:workflowId` | Control active workflow and inspect progress | Pause/resume/cancel, review timeline entries, open checkpoint detail | loading, ready, action-pending, action-success, action-error, timeline-empty |
| `/workflows/:workflowId/checkpoints/:checkpoint` | Inspect checkpoint outcome details | Read checkpoint payload, return to timeline context | loading, ready, not-found, error |
| High-impact gate dialog | Prevent unsafe destructive actions | Confirm/cancel high-risk action, typed phrase for irreversible actions | closed, open-invalid, open-valid, submitting, fail-closed |

## Critical User Flows

### Flow A: Queue Triage and Task Selection
1. User opens `/tasks`.
2. Queue defaults to deterministic ordering (priority -> updated -> task ID).
3. User narrows with status filter and text search.
4. Selecting a row updates detail/actions without route loss.

### Flow B: Task Status Transition (Deterministic)
1. User selects task and target status.
2. UI blocks no-op transitions before request dispatch.
3. Transition request runs with control disabled while pending.
4. Success updates status and feedback; failure shows normalized inline error.

### Flow C: Workflow Control Loop
1. User opens `/workflows/:workflowId`.
2. User triggers `pause` or `resume` based on workflow state.
3. UI prevents duplicate clicks while request is in flight.
4. Feedback row records action, outcome, timestamp, and correlation ID when present.

### Flow D: High-Impact Cancel Gate
1. User initiates workflow cancel or task transition to `cancelled` from active state.
2. Gate dialog opens before dispatch with explicit impact copy.
3. If required typed phrase is incomplete/invalid, submit stays disabled.
4. Confirm dispatches once; cancel closes dialog with no mutation.

### Flow E: Timeline Inspection and Checkpoint Handoff
1. User reviews timeline entries on workflow detail.
2. Entries are rendered in stable order with outcome and decision metadata.
3. User opens checkpoint detail route for deeper context and returns to workflow timeline.

## Interaction Rules and Determinism Constraints
1. Queue sorting is stable: priority rank, then `updated_at`, then task ID.
2. Timeline sorting is stable: checkpoint order, then timestamp, then stable key.
3. Pending controls always expose disabled state and deterministic action label.
4. High-impact actions fail closed when action key, entity ID, or required phrase metadata is missing.
5. Feedback history is bounded (fixed-size list) with deterministic eviction of oldest entries.

## Layout, Hierarchy, and Spacing Guidance

### Desktop (`>= 960px`)
- Use split layout for task queue and task detail/action panel.
- Keep workflow controls above timeline for immediate operational actions.
- Preserve strong visual hierarchy: summary -> action controls -> diagnostic/timeline detail.

### Mobile (`< 960px`)
- Stack summary, queue, detail, and controls in one column.
- Keep destructive actions visually separated from routine controls.
- Ensure long IDs and checkpoint labels wrap without horizontal scroll.

### Spacing and Target Size
- Use spacing rhythm `4/8/12/16/24/32px`.
- Keep interactive targets at least `44x44px`.
- Maintain clear separation between section heading, status metadata, controls, and error text.

## Accessibility Constraints (Non-Negotiable)
1. One primary `h1` per route with ordered subordinate headings.
2. Queue, controls, and timeline regions are exposed with semantic landmarks.
3. Queue rows and action controls are fully keyboard operable.
4. Transition/action errors are announced via `role="alert"`.
5. Action outcome updates use polite live regions and do not steal focus.
6. Confirmation dialogs use `role="dialog"`, `aria-modal`, and managed focus restore.
7. Disabled state reasons for gated/high-risk actions are perceivable to assistive tech.
8. Focus indicators remain visible with WCAG AA contrast.
9. Status/outcome is never conveyed by color alone.
10. Reflow remains usable at `320px` without horizontal page scrolling.

## Content Guidance
- Use explicit labels: `Set status to In Progress`, `Pause workflow`, `Cancel workflow`.
- High-impact copy names irreversible effect directly and references target entity ID.
- Error text should include normalized envelope code and an immediate next step (`Retry`, `Inspect diagnostics`).
- Use consistent status vocabulary across queue chips, detail panes, and dialogs.

## Risks and Mitigations

| Risk | Impact | Mitigation |
| --- | --- | --- |
| Queue density hides priority signals | Slow triage and missed urgent items | Keep summary strip and deterministic priority-first ordering |
| Overly permissive controls cause accidental destructive actions | Incorrect workflow/task state changes | Typed confirmation for high-impact actions and fail-closed metadata checks |
| Inconsistent timeline order creates mistrust | Operators cannot reason about phase progression | Single stable ordering contract and explicit empty state |
| Error visibility is too subtle | Repeat failures and delayed recovery | Inline normalized error plus diagnostics/correlation linkage |

## UX Acceptance Checklist for Implementation Phase
- `/tasks` provides summary, queue list, and actionable detail panel in one workflow.
- Task transitions block no-op and duplicate submissions while pending.
- `/workflows` and `/workflows/:workflowId` expose deterministic run/pause/resume/cancel behavior.
- Workflow timeline renders ordered checkpoints/decisions with explicit empty state.
- High-impact actions require confirmation and typed phrase when configured.
- Failure feedback is visible without devtools and includes correlation context when available.
- All queue/control/gate interactions are keyboard-operable.
- At `320px`, task/workflow control surfaces remain readable with no horizontal page scroll.
