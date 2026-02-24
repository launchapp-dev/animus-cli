# TASK-011 UX Brief: React Shell and Route Architecture

## Phase
- Workflow phase: `ux-research`
- Workflow ID: `1b6b42b5-3ca2-46a7-8a9c-0f2a85c78b85`
- Task: `TASK-011`

## UX Objective
Design a clear, fast-operating web interface for Agent Orchestrator that keeps
operators oriented while they move between daemon control, project context,
task triage, workflow execution, and review handoff.

The shell and routes should make system state legible at a glance, minimize
navigation friction, and remain usable from `320px` mobile widths through
large desktop screens.

## Primary Users and Jobs

| User | Primary jobs | UX success signal |
| --- | --- | --- |
| Operator | Check daemon health, control daemon lifecycle, monitor events, switch active project, run workflows | Can move from dashboard to an action screen in <= 2 navigation steps |
| Delivery lead | Review task and workflow progress within a project context | Can identify active project and current route context without ambiguity |
| Reviewer | Submit handoff review outcome with context | Form submission is clear, validated, and keyboard-operable |

## UX Principles for This Phase
1. Keep context persistent: always show active project context and route location.
2. Prioritize scannability: strong heading hierarchy, compact but readable summaries, predictable action placement.
3. Prefer deterministic state feedback: loading, empty, success, and error states are explicit on every screen.
4. Keep controls accessible by default: keyboard-first operation, visible focus, semantic landmarks.
5. Preserve operational safety: destructive or service-impact actions require explicit confirmation.

## Information Architecture

### Global Navigation Order
1. `Dashboard`
2. `Daemon`
3. `Projects`
4. `Tasks`
5. `Workflows`
6. `Events`
7. `Review Handoff`

### Route Groups
- Global: `/dashboard`, `/daemon`, `/events`, `/reviews/handoff`
- Project-scoped: `/projects`, `/projects/:projectId`, `/projects/:projectId/requirements/:requirementId`
- Task-scoped: `/tasks`, `/tasks/:taskId`
- Workflow-scoped: `/workflows`, `/workflows/:workflowId`, `/workflows/:workflowId/checkpoints/:checkpoint`
- Fallback: `*` (in-app not found)

## Key Screens and Interaction Contracts

| Route | Screen goal | Primary interactions | Required UI states |
| --- | --- | --- | --- |
| `/dashboard` | Provide system and work overview | Refresh summary cards, jump to action routes | loading, stale-data warning, error, no-active-project |
| `/daemon` | Operate daemon lifecycle safely | Start/stop/pause/resume, inspect health and logs, clear logs | loading, command-pending, success toast/inline result, command error |
| `/projects` | Browse/select project context | Select active project, open project detail | loading, empty project list, selection persisted, error |
| `/projects/:projectId` | Understand project scope quickly | View project summary, jump to tasks/workflows/requirements | loading, no-related-items, error |
| `/projects/:projectId/requirements/:requirementId` | Inspect requirement detail in project scope | Read requirement details and linked context | loading, not found, error |
| `/tasks` | Triage work across queues | Filter/sort, open next/prioritized task, inspect stats | loading, empty list, partial-data fallback, error |
| `/tasks/:taskId` | Execute task operations | Update status/checklist/dependencies/assignment | loading, optimistic/pending action, validation error, API error |
| `/workflows` | Monitor and start workflows | View workflow list, trigger run | loading, empty list, run pending, run failure |
| `/workflows/:workflowId` | Control active workflow | Resume/pause/cancel, inspect decisions/checkpoints | loading, transition pending, conflict/error state |
| `/workflows/:workflowId/checkpoints/:checkpoint` | Review checkpoint evidence | Read checkpoint detail, navigate back to workflow | loading, checkpoint missing, error |
| `/events` | Observe live daemon event stream | Connect/reconnect stream, pause autoscroll, copy event details | connecting, live, reconnecting, disconnected, stream error |
| `/reviews/handoff` | Submit review handoff | Fill required form fields, submit review | initial, validation errors, submitting, submitted, submit error |
| `*` | Catch unknown client routes | Present recoverable navigation path | not-found with link back to dashboard |

## Critical User Flows

### Flow A: Startup and Orientation
1. User lands on `/dashboard`.
2. Shell shows app identity, active project badge, and route breadcrumb.
3. Dashboard fetches daemon status and project/task summary endpoints.
4. If no active project exists, dashboard presents a clear action to open `Projects` and set one.

### Flow B: Project Context Selection and Scoped Navigation
1. User opens project selector in header.
2. Selection updates global project context and persists locally.
3. User navigates to `/projects/:projectId` and scoped routes inherit that project context.
4. If route param conflicts with cached selection, route param wins and UI explains current context.

### Flow C: Task Triage and Execution
1. User opens `/tasks` to inspect prioritized and next task sets.
2. User enters `/tasks/:taskId`.
3. Status, assignment, checklist, and dependency actions provide inline pending feedback.
4. On API envelope errors, UI renders deterministic error code/message with non-destructive retry.

### Flow D: Workflow Control
1. User opens `/workflows` to inspect workflow list.
2. User runs workflow or opens `/workflows/:workflowId`.
3. Pause/resume/cancel controls use explicit confirmation where impact is high.
4. Checkpoint details are reachable via `/workflows/:workflowId/checkpoints/:checkpoint`.

### Flow E: Live Event Monitoring
1. User opens `/events` and stream starts in `connecting` state.
2. On open, incoming events append in chronological order using SSE `id` as sequence anchor.
3. On disconnect, UI enters `reconnecting` and retries using `Last-Event-ID`.
4. Persistent failure enters `disconnected` state with manual reconnect control.

### Flow F: Review Handoff Submission
1. User opens `/reviews/handoff`.
2. Form fields are labeled and validated before submit.
3. Submit shows progress state and success/error response message.
4. Success state provides next action links (dashboard/tasks/workflows).

## Layout, Hierarchy, and Spacing Guidance

### Desktop (`>= 960px`)
- Two-region shell: persistent left nav + top header over content column.
- Content width should cap for readability (target ~72-90ch for text-heavy sections).
- Primary actions sit in the top-right of each page header region.

### Mobile (`< 960px`)
- Header with menu button, page title, and project context chip.
- Navigation appears in dismissible drawer with focus trap and escape-close.
- Dense tables/lists collapse into stacked cards with label-value pairs.

### Spacing and Visual Rhythm
- Use a predictable spacing scale (e.g., 4/8/12/16/24/32px).
- Preserve clear separation between: page title, contextual metadata, primary actions, and data regions.
- Keep interaction targets at least `44x44px` on touch layouts.

## Accessibility Constraints (Non-Negotiable)
1. Landmarks: exactly one primary `header`, one primary `nav`, one primary `main` per page.
2. Heading order: `h1` per route view, then descending levels without skipping.
3. Keyboard: all navigation links, drawer controls, and primary actions reachable and operable with keyboard only.
4. Focus: visible focus ring with sufficient contrast on all interactive elements.
5. Contrast: text and UI controls meet WCAG AA contrast expectations (normal text >= 4.5:1).
6. Forms: each input has a programmatic label; errors are announced and tied to fields.
7. SSE updates: event feed uses polite live region semantics and does not steal focus.
8. Motion: respect reduced-motion preference for route/drawer transitions.
9. Reflow: at `320px`, no horizontal page scroll for primary workflows.
10. Recovery: errors are descriptive and include a clear retry or navigation path.

## State and Feedback Model
- Route-level states: `loading`, `ready`, `empty`, `error`.
- Action-level states: `idle`, `pending`, `success`, `failure`.
- Envelope errors always map to consistent UI message structure: code, message, exit code.
- High-impact daemon/workflow actions require confirmation and explicit result feedback.

## Design Risks and Mitigations

| Risk | Impact | Mitigation |
| --- | --- | --- |
| Project context ambiguity across routes | Users act in wrong project scope | Persistent project badge + breadcrumb + precedence rules surfaced in UI |
| Event stream instability | Operators lose timeline continuity | Reconnect strategy with `Last-Event-ID`, visible connection status, manual reconnect |
| Dense operational screens overwhelm users | Slower action completion, errors | Enforce page-level hierarchy and section chunking with clear headings |
| Mobile task/workflow views become unreadable | Reduced usability on small screens | Card-based responsive transforms and action prioritization |

## UX Acceptance Checklist for Implementation Phase
- Every required route has a screen definition with explicit loading/empty/error states.
- Shell displays active project context and route breadcrumb consistently.
- Navigation is equivalent across desktop sidebar and mobile drawer.
- `/events` screen communicates stream connection status and reconnect behavior clearly.
- Review handoff form supports keyboard-only completion and clear validation feedback.
- At `320px`, core flows (project selection, task detail actions, workflow controls) remain usable without horizontal scroll.
