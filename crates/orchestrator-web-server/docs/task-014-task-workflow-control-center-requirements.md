# TASK-014 Requirements: Task/Workflow Control Center Interface

## Phase
- Workflow phase: `requirements`
- Workflow ID: `a72d7b8e-e1e8-4804-b925-355318bca593`
- Task: `TASK-014`

## Objective
Define the implementation contract for a production-ready task/workflow control
center UI that supports:
- queue visibility and prioritization,
- deterministic task state transitions,
- workflow run/pause/resume/cancel controls,
- phase/checkpoint timeline visibility,
- explicit gating UX for high-impact actions.

## Existing Baseline
- Routes exist for task/workflow pages:
  - `/tasks`
  - `/tasks/:taskId`
  - `/workflows`
  - `/workflows/:workflowId`
  - `/workflows/:workflowId/checkpoints/:checkpoint`
- Current task/workflow screens render mostly raw JSON payload panels and do not
  provide operator-focused queue controls.
- Web server already exposes task/workflow mutation endpoints under `/api/v1/*`.
- API client currently covers task/workflow read paths but not complete control
  flows (for example workflow run/pause/resume/cancel and task transition
  actions).
- Diagnostics + correlation support exists via TASK-019 and can be reused for
  task/workflow actions.

## Scope
In scope for implementation after this phase:
- Build queue-first task/workflow control surfaces in existing task/workflow
  routes.
- Add task transition controls with deterministic status handling.
- Add workflow control actions (run, pause, resume, cancel) with explicit
  action-state feedback.
- Add workflow phase timeline UI using checkpoints/decisions data.
- Add high-impact gating UX for destructive or irreversible actions.
- Add tests for queue rendering, transitions, workflow controls, timeline
  ordering, and gating behavior.

Out of scope for TASK-014:
- Replacing shell navigation architecture.
- Introducing new desktop-wrapper dependencies.
- Editing `.ao` state files directly.
- Rewriting workflow engine/state-machine behavior in `orchestrator-core`.
- Adding external storage or third-party telemetry services.

## Non-Negotiable Constraints
- Keep endpoint usage under `/api/v1`.
- Preserve `ao.cli.v1` envelope parsing and existing error normalization.
- Keep state changes service-driven through API handlers (no direct `.ao` edits).
- Keep the UI keyboard operable with visible focus states.
- Keep control-center flows usable at `320px` width without horizontal page
  scrolling.
- Keep action behavior deterministic:
  - no duplicate submissions while pending,
  - explicit loading/disabled states for in-flight actions,
  - stable sorting rules for queue and timeline.

## Required API Coverage (Client-Side)
TASK-014 implementation must consume existing server endpoints and add client
wrappers where missing:

Task queue and transitions:
- `GET /api/v1/tasks`
- `GET /api/v1/tasks/prioritized`
- `GET /api/v1/tasks/next`
- `GET /api/v1/tasks/stats`
- `GET /api/v1/tasks/:id`
- `PATCH /api/v1/tasks/:id`
- `POST /api/v1/tasks/:id/status`
- `POST /api/v1/tasks/:id/checklist`
- `PATCH /api/v1/tasks/:id/checklist/:item_id`
- `POST /api/v1/tasks/:id/dependencies`
- `DELETE /api/v1/tasks/:id/dependencies/:dependency_id`

Workflow controls and timeline:
- `GET /api/v1/workflows`
- `POST /api/v1/workflows/run`
- `GET /api/v1/workflows/:id`
- `GET /api/v1/workflows/:id/decisions`
- `GET /api/v1/workflows/:id/checkpoints`
- `GET /api/v1/workflows/:id/checkpoints/:checkpoint`
- `POST /api/v1/workflows/:id/resume`
- `POST /api/v1/workflows/:id/pause`
- `POST /api/v1/workflows/:id/cancel`

## Functional Requirements

### FR-01: Queue-Centric Task View
- Present tasks in a queue-focused layout with clear hierarchy:
  - summary strip (counts/status),
  - primary queue list,
  - selected-task detail/action pane.
- Default ordering must be deterministic:
  - priority-weighted first, then most recently updated, then task ID.
- Provide at minimum:
  - status filter,
  - text search,
  - quick link to task detail route.
- Queue empty/error/loading states must be explicit and recoverable.

### FR-02: Task State Transition Controls
- Support operator-triggered transitions for allowed status values:
  - `backlog|todo`, `ready`, `in-progress|in_progress`, `blocked`,
    `on-hold|on_hold`, `done`, `cancelled`.
- Transition dispatch must use explicit action endpoints (`POST /tasks/:id/status`
  or targeted patch flow where required by payload shape).
- Transition controls must:
  - disable while request is pending,
  - reject no-op transitions in the UI before request dispatch,
  - surface normalized API errors inline with actionable message.

### FR-03: Workflow Control Actions
- Provide action controls for:
  - run workflow,
  - pause workflow,
  - resume workflow,
  - cancel workflow.
- Workflow action visibility must reflect workflow status where possible (for
  example hide/disable pause on terminal workflows).
- Result feedback must include action, status, timestamp, and correlation ID
  when available.

### FR-04: Workflow Phase Timeline
- Render a timeline view in workflow detail using decisions/checkpoints data.
- Timeline ordering must be stable and deterministic:
  - checkpoint index/order ascending,
  - ties resolved by timestamp then stable key.
- Each timeline entry should expose:
  - phase/checkpoint identifier,
  - status/outcome,
  - decision metadata when available,
  - timestamp.
- Missing timeline data must render a clear empty state rather than raw JSON.

### FR-05: Gating UX for High-Impact Actions
- Add confirmation gating before high-impact actions:
  - workflow cancel,
  - task delete (if exposed in TASK-014 UI),
  - task transition to `cancelled` from active states.
- Gating rules:
  - high-impact actions require explicit confirmation step in modal/dialog.
  - irreversible actions require typed confirmation phrase.
  - user can cancel safely without dispatch.
- Gating must fail closed if required metadata is missing (action key, expected
  phrase, target entity ID).

### FR-06: Diagnostics and Auditable Feedback
- Integrate action flows with existing diagnostics and telemetry plumbing.
- Task/workflow action failures must remain visible in UI without devtools.
- Action feedback history must be bounded and deterministic (fixed-size in-memory
  list with oldest-first eviction).

### FR-07: Accessibility and Responsive Usability
- Use semantic headings/landmarks for queue, controls, and timeline regions.
- Ensure keyboard-only completion for:
  - selecting queued tasks,
  - applying status transitions,
  - running/pausing/resuming/canceling workflows,
  - completing/canceling high-impact confirmation gates.
- At mobile widths (`<960px` down to `320px`), queue/detail/controls stack and
  remain readable without horizontal scrolling.

## Non-Functional Requirements

### NFR-01: Correctness and Safety
- Control actions must be idempotent at UI level for repeated clicks while
  pending.
- UI state should only commit success feedback after successful API envelope
  responses.
- Any decode/contract mismatch must fail safely with explicit error state.

### NFR-02: Performance
- Queue and timeline rendering should remain responsive for at least 200 items
  in local state.
- Action feedback/timeline lists must use bounded retention to avoid unbounded
  memory growth.

## Acceptance Criteria
- `AC-01`: `/tasks` presents queue-focused UI with summary, list, and actionable
  detail region.
- `AC-02`: Task status transitions execute through API actions and reflect
  updated status after refresh.
- `AC-03`: UI blocks duplicate task transition submission while pending.
- `AC-04`: `/workflows` and `/workflows/:workflowId` expose run/pause/resume/
  cancel controls with clear success/error feedback.
- `AC-05`: Workflow detail shows ordered phase/checkpoint timeline from API
  data.
- `AC-06`: High-impact actions require explicit gating and do not dispatch when
  gate is incomplete.
- `AC-07`: Task/workflow action failures flow through normalized envelope error
  handling and diagnostics metadata.
- `AC-08`: Task/workflow control surfaces are keyboard operable and maintain
  visible focus indicators.
- `AC-09`: Control-center layouts remain usable at `320px` without horizontal
  page scrolling.
- `AC-10`: Existing route shell behavior and `api_only` mode remain unchanged.

## Testable Acceptance Checklist
- `T-01`: Component test for queue sort/filter behavior and empty/error/loading
  states.
- `T-02`: Component/integration test for task status transition success + error
  handling.
- `T-03`: Test verifies pending-state duplicate click suppression on task and
  workflow actions.
- `T-04`: Component test verifies workflow action controls map to correct API
  calls and action labels.
- `T-05`: Component test verifies timeline ordering and stable rendering with
  mixed checkpoint/decision timestamps.
- `T-06`: Gating tests verify typed confirmation behavior for high-impact
  actions.
- `T-07`: Accessibility test verifies keyboard interaction and semantic
  structure for queue/controls/timeline.
- `T-08`: Responsive viewport test verifies no horizontal overflow at `320px`.

## Verification Matrix
| Requirement | Verification method |
| --- | --- |
| Queue clarity and deterministic ordering | UI component tests + route-level smoke checks |
| Task transition correctness | API client tests + transition flow tests |
| Workflow controls | action dispatch tests + integration checks |
| Timeline ordering and readability | timeline renderer tests + fixture snapshots |
| High-impact gating UX | modal/gate behavior tests |
| Accessibility and mobile behavior | keyboard assertions + viewport overflow checks |
| Envelope/error compatibility | existing `client/envelope` regression tests |
