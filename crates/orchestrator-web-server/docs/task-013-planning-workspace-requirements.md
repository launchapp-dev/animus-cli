# TASK-013 Requirements: Vision/Requirements Planning Workspace UI

## Phase
- Workflow phase: `requirements`
- Workflow ID: `65737bd7-e019-4524-b446-a9de9e082165`
- Task: `TASK-013`

## Objective
Define the implementation contract for a production-ready planning workspace UI
that supports:
- vision authoring
- vision/requirement refinement actions
- requirements list/detail browsing
- deterministic deep-link flows across planning surfaces

## Existing Baseline Audit
- React shell, route architecture, shared envelope-aware API client, and SSE
  support were delivered in TASK-011.
- Current web routes include project requirement deep links
  (`/projects/:projectId/requirements/:requirementId`) but no dedicated planning
  workspace routes.
- Web API currently exposes requirement read endpoints only:
  - `GET /api/v1/requirements`
  - `GET /api/v1/requirements/:id`
  - `GET /api/v1/project-requirements/*`
- Planning mutation flows exist in core/CLI behavior but are not currently
  exposed as web API routes.

## Scope
In scope for implementation after this phase:
- Add planning workspace routes to the web UI with deep-link support.
- Provide vision authoring form with save/update behavior.
- Provide requirements list + detail views designed for planning work.
- Provide requirement authoring/editing/deletion UI flows.
- Provide requirement refinement trigger flow (single or multi-select scope).
- Add server routes and web API handlers needed by those UI mutations.
- Preserve and extend existing shared envelope/client/error handling patterns.

Out of scope for TASK-013:
- Replacing task/workflow/project shell architecture from TASK-011.
- Redesigning `.ao` storage formats.
- Adding desktop frameworks or non-Rust server dependencies.
- Changing daemon event stream contracts.

## Non-Negotiable Constraints
- Keep all endpoint behavior under `/api/v1`.
- Preserve `ao.cli.v1` envelope semantics for all JSON responses.
- Keep `api_only=true` behavior unchanged (`/` and `/*path` stay API-only).
- Do not directly edit `.ao/*.json`; all state changes must go through AO
  service APIs surfaced by server handlers.
- Keep planning UI keyboard-operable with visible focus states.
- Keep planning views usable at `320px` width without horizontal page scroll.

## Planned Route Contract

| Route | Purpose | Primary endpoint dependencies |
| --- | --- | --- |
| `/planning` | Planning landing route | client redirect to `/planning/vision` |
| `/planning/vision` | Vision authoring and refinement entrypoint | `GET /api/v1/vision`, `POST /api/v1/vision`, `POST /api/v1/vision/refine` |
| `/planning/requirements` | Requirements workspace list and batch actions | `GET /api/v1/requirements`, `POST /api/v1/requirements`, `POST /api/v1/requirements/draft`, `POST /api/v1/requirements/refine` |
| `/planning/requirements/new` | New requirement authoring form | `POST /api/v1/requirements` |
| `/planning/requirements/:requirementId` | Requirement detail/edit/refine/delete | `GET /api/v1/requirements/:id`, `PATCH /api/v1/requirements/:id`, `DELETE /api/v1/requirements/:id`, `POST /api/v1/requirements/refine` |

Deep-link requirements:
- Browser refresh on any planning route must boot SPA and render the same view.
- Requirement detail links must preserve exact `requirementId` without case
  mutation.
- `/projects/:projectId/requirements/:requirementId` must expose a clear link to
  `/planning/requirements/:requirementId`.

## Required API Additions for TASK-013

### Vision Endpoints
- `GET /api/v1/vision`
  - returns current vision document or `null` in envelope `data`.
- `POST /api/v1/vision`
  - creates or updates vision using `VisionDraftInput`-compatible payload.
- `POST /api/v1/vision/refine`
  - refines existing vision and returns updated vision plus refinement metadata.

### Requirement Endpoints
- Keep existing:
  - `GET /api/v1/requirements`
  - `GET /api/v1/requirements/:id`
- Add:
  - `POST /api/v1/requirements` (create)
  - `PATCH /api/v1/requirements/:id` (update selected mutable fields)
  - `DELETE /api/v1/requirements/:id` (delete)
  - `POST /api/v1/requirements/draft` (draft generation flow)
  - `POST /api/v1/requirements/refine` (refine selected/all requirements)

### Server Layer Constraint
Vision refinement logic currently lives in CLI planning ops modules; web-server
implementation must avoid cross-crate inversion. TASK-013 implementation may:
- extract reusable refinement logic into `orchestrator-core`, or
- implement a server-native refinement path with equivalent deterministic output.

Any selected approach must keep behavior testable and avoid importing
`orchestrator-cli` into web API/server crates.

## UI Behavior Contract
Vision workspace behavior:
- Render explicit empty-state when no vision exists.
- Authoring form includes at minimum:
  - project name
  - problem statement
  - target users
  - goals
  - constraints
  - value proposition
- Save action is disabled while request is in flight.
- Validation errors and API envelope errors are shown inline and do not clear
  entered form values.

Requirements workspace behavior:
- List view defaults to stable ordering by requirement ID.
- List provides clear entry to:
  - open detail
  - create new requirement
  - trigger refinement for selected/all requirements
- Detail view supports edit and delete operations with deterministic confirmation
  flow before delete.
- `not_found` for deleted/missing requirement deep links renders a recoverable UI
  state (link back to list).

Cross-surface behavior:
- All screens use shared API client and envelope parser; no raw endpoint
  responses are consumed directly.
- Loading, empty, and error states are explicit per route-level data dependency.

## Accessibility and Responsive Contract
- Planning screens include semantic landmarks and ordered heading structure.
- Form controls include programmatic labels.
- Keyboard-only flow must cover:
  - opening planning routes
  - creating/updating/deleting requirements
  - saving/refining vision
- At small widths (`<= 960px` and down to `320px`), planning forms and list
  actions stack without clipping controls.

## Acceptance Criteria
- `AC-01`: Planning routes (`/planning`, `/planning/vision`,
  `/planning/requirements`, `/planning/requirements/new`,
  `/planning/requirements/:requirementId`) load via direct navigation when UI is
  enabled.
- `AC-02`: A user can create/update a vision document from the web UI and see
  persisted values after refresh.
- `AC-03`: A user can create, update, and delete requirements from the planning
  workspace.
- `AC-04`: Requirement refinement flow updates targeted requirements and returns
  deterministic result payload to UI.
- `AC-05`: Requirements list and detail routes deep-link correctly, including
  deep links from project requirement pages.
- `AC-06`: All planning requests use envelope-aware API client parsing and map
  errors to consistent UI model (`code`, `message`, `exitCode`).
- `AC-07`: Planning screens meet keyboard/focus and mobile usability baseline.
- `AC-08`: `api_only=true` behavior remains unchanged for root/static handlers.

## Testable Acceptance Checklist
- `AC-T1`: Route registry and nav tests include planning entries and pass.
- `AC-T2`: API client tests cover new planning endpoints and envelope error
  mapping.
- `AC-T3`: UI tests cover planning list/detail deep links and not-found handling.
- `AC-T4`: Server tests verify new planning routes under `/api/v1/*` and keep
  `api_only` static behavior intact.
- `AC-T5`: At least one accessibility-focused test/assertion verifies labeled
  form controls and keyboard access for planning primary actions.

## Verification Matrix

| Requirement | Verification method |
| --- | --- |
| Planning deep links and route coverage | Router tests + browser/manual path checks |
| Vision authoring correctness | API handler tests + UI integration save/load check |
| Requirement CRUD and refinement | Web API tests + UI mutation flow tests |
| Envelope/error consistency | Unit tests in `src/lib/api/*` |
| Accessibility/responsive baseline | Keyboard/semantic assertions + viewport smoke checks |
| `api_only` compatibility | Existing server tests plus regression coverage around `/` and `/*path` |

## Deterministic Deliverables for Implementation Phase
- Planning route and screen modules under `web-ui/src/app` and/or
  `web-ui/src/features/planning`.
- Shared API client additions for planning endpoints with typed decoders.
- Web API service handlers and web server routes for vision and requirement
  mutations required by this document.
- Updated OpenAPI spec for all added planning endpoints.
- Tests for route coverage, API contract parsing, and core planning UI flows.
