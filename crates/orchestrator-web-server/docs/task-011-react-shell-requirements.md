# TASK-011 Requirements: React Shell and Route Architecture

## Phase
- Workflow phase: `requirements`
- Workflow ID: `1b6b42b5-3ca2-46a7-8a9c-0f2a85c78b85`
- Task: `TASK-011`

## Objective
Define a production-ready React app shell and route architecture that sits on top
of `orchestrator-web-server` and consumes existing `/api/v1` endpoints with
consistent state, navigation, and error handling.

## Existing Baseline
- `orchestrator-web-server` already serves SPA assets and falls back to
  `index.html` for unknown static paths.
- API surface is already available under `/api/v1/*` and described by
  `crates/orchestrator-web-server/openapi.json`.
- SSE stream exists at `/api/v1/events`.
- `api_only` mode must continue to return API responses without UI routes.

## Scope
In scope for implementation that follows this requirements phase:
- React shell with a persistent layout frame and primary navigation.
- Route tree for dashboard, daemon, projects, tasks, workflows, and reviews.
- Project context frame that shows and switches active project context.
- Shared API client that unwraps `ao.cli.v1` envelopes consistently.
- Route-level loading, empty, and error states.
- Basic SSE events view and connection lifecycle handling.
- Baseline spacing and typography primitives that keep hierarchy readable on
  desktop and mobile.

Out of scope for this task:
- Redesigning backend endpoint contracts.
- Replacing Swagger docs page behavior at `/api/v1/docs`.
- Adding desktop-wrapper frameworks.
- Full visual polish pass beyond usable, accessible baseline UI.

## Constraints
- Keep all behavior compatible with current `orchestrator-web-server` routing and
  static asset serving.
- Preserve Rust-only workspace policy and avoid desktop shell dependencies.
- Preserve `api_only` mode semantics.
- Keep all endpoint calls under `/api/v1`.
- Ensure keyboard access, focus visibility, and semantic landmarks.
- Ensure usable layout from small screens to desktop widths.
- Keep static hosting compatibility with current fallback behavior (`/*path` to
  `index.html` when asset is missing).
- Keep SSE compatibility with current server event contract:
  - event type: `daemon-event`
  - event `id`: monotonic `seq`
  - replay support via `Last-Event-ID` request header.

## Server-Derived Behavior Contracts
- `GET /`:
  - when `api_only=false`, serve `index.html` (disk override or embedded asset);
  - when `api_only=true`, return `ao.cli.v1` success envelope with `api_base`.
- `GET /*path`:
  - when `api_only=false`, serve requested asset when present, else fall back to
    `index.html`;
  - when `api_only=true`, return not-found `ao.cli.v1` envelope.
- Static path handling must remain sanitized against traversal (`..`, root/prefix
  components) and preserve current normalization semantics.
- API responses for operational endpoints remain envelope-based (`ok` + `data` on
  success, `ok=false` + `error` on failure).

## Proposed Route Tree

| Route | Purpose | Primary endpoint dependencies |
| --- | --- | --- |
| `/` | Redirect to `/dashboard` | None |
| `/dashboard` | Global status overview | `GET /api/v1/system/info`, `GET /api/v1/daemon/status`, `GET /api/v1/projects/active`, `GET /api/v1/tasks/stats` |
| `/daemon` | Daemon status and controls | `GET /api/v1/daemon/status`, `GET /api/v1/daemon/health`, `GET/DELETE /api/v1/daemon/logs`, `POST /api/v1/daemon/{start|stop|pause|resume}`, `GET /api/v1/daemon/agents` |
| `/projects` | Project list and selection | `GET /api/v1/projects`, `GET /api/v1/projects/active`, `GET /api/v1/project-requirements` |
| `/projects/:projectId` | Project summary panel | `GET /api/v1/projects/{id}`, `GET /api/v1/projects/{id}/tasks`, `GET /api/v1/projects/{id}/workflows`, `GET /api/v1/project-requirements/{id}` |
| `/projects/:projectId/requirements/:requirementId` | Requirement detail in project scope | `GET /api/v1/project-requirements/{project_id}/{requirement_id}` |
| `/tasks` | Task explorer and filters | `GET /api/v1/tasks`, `GET /api/v1/tasks/prioritized`, `GET /api/v1/tasks/next`, `GET /api/v1/tasks/stats` |
| `/tasks/:taskId` | Task detail and operations | `GET/PATCH/DELETE /api/v1/tasks/{id}`, `POST /api/v1/tasks/{id}/status`, `POST /api/v1/tasks/{id}/assign-agent`, `POST /api/v1/tasks/{id}/assign-human`, `POST /api/v1/tasks/{id}/checklist`, `PATCH /api/v1/tasks/{id}/checklist/{item_id}`, `POST /api/v1/tasks/{id}/dependencies`, `DELETE /api/v1/tasks/{id}/dependencies/{dependency_id}` |
| `/workflows` | Workflow list and run entrypoint | `GET /api/v1/workflows`, `POST /api/v1/workflows/run` |
| `/workflows/:workflowId` | Workflow detail and controls | `GET /api/v1/workflows/{id}`, `GET /api/v1/workflows/{id}/decisions`, `GET /api/v1/workflows/{id}/checkpoints`, `POST /api/v1/workflows/{id}/{resume|pause|cancel}` |
| `/workflows/:workflowId/checkpoints/:checkpoint` | Checkpoint detail | `GET /api/v1/workflows/{id}/checkpoints/{checkpoint}` |
| `/events` | Live daemon event stream view | `GET /api/v1/events` |
| `/reviews/handoff` | Handoff review form | `POST /api/v1/reviews/handoff` |
| `*` | UI 404 page | None |

## Navigation and Layout Contract
- Primary navigation order: `Dashboard`, `Daemon`, `Projects`, `Tasks`,
  `Workflows`, `Events`, `Review Handoff`.
- Shell must persist across route changes and include:
  - app title/identity
  - project context selector + active project badge
  - route breadcrumb segment
- Desktop (`>= 960px`): left navigation + top header + scrollable main content.
- Mobile (`< 960px`): top header + menu button that opens the same nav items in
  a dismissible drawer.
- Route transitions must preserve scroll position behavior deterministically:
  reset to top on section change, preserve within same detail section when
  feasible.

## Project Context Frame
- Header includes project selector, active project badge, and route breadcrumbs.
- Context state source order:
  - explicit route param project id
  - cached user selection
  - `/api/v1/projects/active`
- Child views that depend on project context must render deterministic empty-state
  messaging if no project is active.

## Data and Error Contract
- All API fetches go through a shared client that enforces `ao.cli.v1` envelope.
- `ok: false` responses are mapped to typed UI errors using `error.code`,
  `error.message`, and `error.exit_code`.
- Route loaders/components must never assume raw payload shape before envelope
  validation.
- Events stream client should track the latest `id` and reconnect with
  `Last-Event-ID` header when reconnecting.
- The client must expose a single response shape for components:
  - success: `{ kind: "ok", data }`
  - failure: `{ kind: "error", code, message, exitCode }`

## Accessibility and Responsive Behavior
- Use semantic landmarks: `header`, `nav`, `main`, and section headings in order.
- All primary navigation actions and control buttons must be keyboard operable.
- Focus states must remain visible at all times.
- Content must remain usable at `320px` width without horizontal page scrolling.
- Tables/lists with dense data must have mobile-friendly wrapping or stacked
  presentation.

## Acceptance Criteria
- Deep-linking to every defined UI route loads without server 404 when web UI is
  enabled.
- Unknown UI routes render app-level not-found view while preserving shell.
- Every route listed above maps to at least one implemented screen component.
- The shared API client is used by all route data fetches and handles both success
  and error envelopes.
- Project context frame is visible on all project-scoped pages and reflects active
  project deterministically.
- Daemon events page connects to SSE and displays streamed records with reconnect
  behavior.
- Base shell passes keyboard navigation sanity checks and basic screen reader
  semantics.
- `api_only` mode still returns API-only responses and does not attempt UI shell
  rendering.

## Testable Acceptance Checklist
- `AC-01`: For each route in the proposed route tree, direct browser navigation
  returns `200` and boots the SPA when `api_only=false`.
- `AC-02`: Route misses in the client route table render an in-app not-found
  screen while preserving persistent shell chrome.
- `AC-03`: Every route-level fetch uses one shared envelope parser; no screen
  reads raw `fetch().json()` payload without envelope validation.
- `AC-04`: Error rendering maps `error.code`, `error.message`, and
  `error.exit_code` into a consistent UI error model.
- `AC-05`: Project-scoped screens derive active context by precedence:
  `route param -> cached selection -> /api/v1/projects/active`.
- `AC-06`: SSE stream consumer resumes from last seen sequence using
  `Last-Event-ID` and tolerates transient disconnect/reconnect.
- `AC-07`: Keyboard-only user can open navigation, move between links, activate
  primary actions, and reach main heading on each page.
- `AC-08`: At `320px` width, navigation and primary content remain usable
  without horizontal page scroll.
- `AC-09`: `api_only=true` keeps existing behavior (`GET /` envelope response,
  `GET /*path` not found, API endpoints unchanged).

## Acceptance Verification Matrix
| Requirement | Verification method |
| --- | --- |
| UI deep links work | Browser check for each route plus server test coverage for static fallback path behavior |
| Envelope handling is centralized | Unit tests around API client parsing success and error envelopes |
| Project context is deterministic | Unit/integration tests for precedence order (route param, cached selection, active project endpoint) |
| SSE reconnect behavior works | Integration test or mocked EventSource test validating reconnect with latest event id |
| Accessibility baseline is present | Keyboard-only navigation pass + semantic landmark assertions in component tests |
| `api_only` behavior is unchanged | Server test against `/` and `/*path` when `api_only=true`, ensuring JSON API response or not-found envelope |

## Implementation Notes (Next Phase Input)
- Suggested frontend source location:
  `crates/orchestrator-web-server/web-ui/`.
- Suggested module layout:
  - `src/app/` for shell, providers, and route definitions.
  - `src/features/` for domain screens (`daemon`, `projects`, `tasks`,
    `workflows`, `reviews`, `events`).
  - `src/lib/api/` for typed client and envelope parsing.
  - `src/lib/ui/` for shared layout/navigation primitives.
- Keep asset build output wired to current embedded/static serving flow used by
  `orchestrator-web-server`.
- Do not hand-edit hashed build files in `embedded/assets`; regenerate through
  the frontend build pipeline and check in deterministic outputs.
- Preserve `/api/v1/docs` and `/api/v1/openapi.json` behavior unchanged.
- Preserve `assets_dir` override behavior: disk assets must take precedence over
  embedded assets in development/override mode.

## Deterministic Deliverables for Implementation Phase
- Add a checked-in frontend source tree under
  `crates/orchestrator-web-server/web-ui/`.
- Add a route definition module that includes all routes in this document.
- Add a shared API client module used by every route-level data fetch.
- Add component-level not-found and error boundary handling.
- Add baseline tests for envelope parsing and route rendering.
