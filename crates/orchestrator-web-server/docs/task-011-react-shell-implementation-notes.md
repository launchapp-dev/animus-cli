# TASK-011 Implementation Notes: React Shell and Route Architecture

## Purpose
Translate requirements into deterministic implementation slices for the next
phase without changing server endpoint contracts.

## Non-Negotiable Constraints
- Keep all API calls under `/api/v1`.
- Do not modify `.ao` state files directly.
- Preserve `api_only` behavior in `orchestrator-web-server`.
- Preserve embedded static hosting and fallback semantics.
- Keep `/api/v1/docs` and `/api/v1/openapi.json` unchanged.

## Proposed Source Layout
- `crates/orchestrator-web-server/web-ui/`
- `crates/orchestrator-web-server/web-ui/src/app/`
- `crates/orchestrator-web-server/web-ui/src/features/`
- `crates/orchestrator-web-server/web-ui/src/lib/api/`
- `crates/orchestrator-web-server/web-ui/src/lib/ui/`

## Route Architecture Notes
- Centralize route declarations in a single module under `src/app/`.
- Persist shell frame across route transitions.
- Route groups:
  - global: `/dashboard`, `/daemon`, `/events`, `/reviews/handoff`
  - project: `/projects`, `/projects/:projectId`,
    `/projects/:projectId/requirements/:requirementId`
  - task: `/tasks`, `/tasks/:taskId`
  - workflow: `/workflows`, `/workflows/:workflowId`,
    `/workflows/:workflowId/checkpoints/:checkpoint`
- Include explicit client-side not-found route.

## API Client Contract
- Provide one client entrypoint for JSON API requests.
- Parse and validate `ao.cli.v1` envelope before exposing payload to screens.
- Return a normalized union shape:
  - success: `{ kind: "ok", data }`
  - error: `{ kind: "error", code, message, exitCode }`
- Keep per-endpoint helpers thin wrappers around shared request+envelope logic.

## Project Context Notes
- Build a project context provider in `src/app/` or `src/features/projects/`.
- Precedence order:
  - route param
  - cached selection
  - `/api/v1/projects/active`
- Expose context helpers to pages needing project-scoped data.

## SSE Notes
- Implement a stream client for `/api/v1/events`.
- Persist last seen sequence from SSE `id`.
- Reconnect using `Last-Event-ID` header.
- Surface connection states in UI: connecting, open, retrying, failed.

## Accessibility and Responsive Notes
- Landmarks: one `header`, one `nav`, one `main`.
- Visible focus indicator on all interactive elements.
- Mobile-first layout (`320px` baseline), then desktop split layout.
- Navigation drawer and desktop sidebar must expose the same destinations.

## Suggested Implementation Sequence
1. Create frontend source tree and app bootstrap.
2. Add shell layout and route registry.
3. Add shared API client + envelope parser tests.
4. Add project context provider and integrate route params.
5. Add placeholder screens for every required route.
6. Wire SSE events view with reconnect behavior.
7. Add route-level loading/empty/error states.
8. Add baseline accessibility and responsive checks.
9. Build assets and wire deterministic embedded output for server hosting.

## Validation Targets for Implementation Phase
- Route deep-link coverage for all required client routes.
- Unit tests for envelope parsing success/failure paths.
- Integration test or mocked tests for SSE reconnect behavior.
- Smoke test for `api_only=true` root and wildcard behavior remaining intact.
