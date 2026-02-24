# TASK-011 Wireframes: React Shell and Route Architecture

These mockups define concrete, production-oriented wireframes for the standalone
daemon web UI shell and route system described in `TASK-011`.

## Files
- `wireframes.html`: visual wireframe boards for desktop + mobile layouts and
  route/state examples.
- `wireframes.css`: shared styles used by the wireframe boards.
- `route-architecture.tsx`: React-oriented route tree and shell composition
  skeleton for implementation handoff.

## Route Coverage

| Route | Covered in |
| --- | --- |
| `/` | `wireframes.html` (`Root Redirect`) + `route-architecture.tsx` |
| `/dashboard` | `wireframes.html` (`Dashboard`) + `route-architecture.tsx` |
| `/daemon` | `wireframes.html` (`Daemon`) + `route-architecture.tsx` |
| `/projects` | `wireframes.html` (`Projects`) + `route-architecture.tsx` |
| `/projects/:projectId` | `wireframes.html` (`Project Detail`) + `route-architecture.tsx` |
| `/projects/:projectId/requirements/:requirementId` | `wireframes.html` (`Requirement Detail`) + `route-architecture.tsx` |
| `/tasks` | `wireframes.html` (`Tasks`) + `route-architecture.tsx` |
| `/tasks/:taskId` | `wireframes.html` (`Task Detail`) + `route-architecture.tsx` |
| `/workflows` | `wireframes.html` (`Workflows`) + `route-architecture.tsx` |
| `/workflows/:workflowId` | `wireframes.html` (`Workflow Detail`) + `route-architecture.tsx` |
| `/workflows/:workflowId/checkpoints/:checkpoint` | `wireframes.html` (`Checkpoint Detail`) + `route-architecture.tsx` |
| `/events` | `wireframes.html` (`Events`) + `route-architecture.tsx` |
| `/reviews/handoff` | `wireframes.html` (`Review Handoff`) + `route-architecture.tsx` |
| `*` | `wireframes.html` (`In-App Not Found`) + `route-architecture.tsx` |

## Mockup-Review Resolutions
- Added root redirect representation (`/ -> /dashboard`) to close route matrix gap.
- Added explicit project context selector and precedence note:
  `route param -> cached selection -> /api/v1/projects/active`.
- Added accessible review handoff form wireframe with label/error associations.
- Added explicit events live-region semantics (`aria-live="polite"`) and reconnect cues.
- Increased touch-target affordances to 44px minimum for primary mobile controls.
- Added reduced-motion handling and no-horizontal-scroll reinforcement for mobile.
- Tightened `route-architecture.tsx` with deterministic envelope parsing and AC trace metadata.

## State Coverage
- Route-level: `loading`, `ready`, `empty`, `error`
- Action-level: `idle`, `pending`, `success`, `failure`
- Stream-level (`/events`): `connecting`, `live`, `reconnecting`, `disconnected`

## Acceptance Criteria Traceability

| AC | Trace |
| --- | --- |
| `AC-01` | Route cards in `wireframes.html` (including `/` redirect) + `routeCoverage` in `route-architecture.tsx` |
| `AC-02` | In-app not-found card in `wireframes.html` + wildcard route in `wireframeRouter` |
| `AC-03` | `parseAoEnvelope` contract and route-level API usage notes in `route-architecture.tsx` |
| `AC-04` | Envelope error render board in `wireframes.html` + normalized `ApiResult` mapping in `route-architecture.tsx` |
| `AC-05` | Project context controls in desktop/mobile wireframes + `resolveProjectContext` precedence logic |
| `AC-06` | SSE continuity board (`Last-Event-ID`) and events live-region panel in `wireframes.html` |
| `AC-07` | Landmark shell composition in wireframes + keyboard focus styles in `wireframes.css` |
| `AC-08` | Mobile 320px board, drawer behavior, and responsive CSS constraints in `wireframes.css` |
| `AC-09` | Explicit server-only `api_only` note in `acceptanceTraceability` (`route-architecture.tsx`) |

## Accessibility and Responsive Intent
- Shell landmarks modeled as `header`, `nav`, and `main`.
- Desktop board uses persistent sidebar + header.
- Mobile board uses menu button + drawer-equivalent navigation.
- Buttons/controls are shown with visible focus style in wireframe examples.
