# TASK-013 Implementation Notes: Planning Workspace UI

## Purpose
Translate TASK-013 planning workspace requirements into concrete implementation
slices across web UI, web API, and server routing while preserving current
shell and API-only behavior.

## Non-Negotiable Constraints
- Keep changes scoped to web planning surfaces and required API endpoints.
- Preserve `ao.cli.v1` response envelope for all JSON endpoints.
- Preserve `api_only` behavior and static fallback semantics.
- Keep all `.ao` mutations inside service-driven handlers; do not write state
  files directly.
- Do not introduce dependency edges from web API/server crates to
  `orchestrator-cli`.

## Proposed Change Surface

### 1. UI route and navigation updates
- `crates/orchestrator-web-server/web-ui/src/app/router.tsx`
  - add planning routes:
    - `/planning`
    - `/planning/vision`
    - `/planning/requirements`
    - `/planning/requirements/new`
    - `/planning/requirements/:requirementId`
- `crates/orchestrator-web-server/web-ui/src/app/shell.tsx`
  - add `Planning` nav item in deterministic order.
- `crates/orchestrator-web-server/web-ui/src/app/screens.tsx`
  - add or split planning screens from generic JSON panels into focused views.

### 2. Planning API client and contracts
- `crates/orchestrator-web-server/web-ui/src/lib/api/client.ts`
  - add methods for:
    - vision get/save/refine
    - requirements create/update/delete/draft/refine
- `crates/orchestrator-web-server/web-ui/src/lib/api/contracts/models.ts`
  - add typed models for new planning request/response payloads.
- `crates/orchestrator-web-server/web-ui/src/lib/api/contracts/guards.ts`
  - add decoder guards for new planning payloads.
- extend existing tests:
  - `client.test.ts`
  - `guards.test.ts`
  - `normalize.test.ts` (if response normalization changes)

### 3. Planning-focused UI modules
- Add planning-focused feature modules:
  - `crates/orchestrator-web-server/web-ui/src/features/planning/vision/*`
  - `crates/orchestrator-web-server/web-ui/src/features/planning/requirements/*`
- Keep reusable state utilities in `src/lib/api/` and avoid endpoint-specific
  parsing in component files.

### 4. Web server/API route additions
- `crates/orchestrator-web-server/src/services/web_server.rs`
  - add handler wiring for planning mutation endpoints:
    - `GET /api/v1/vision`
    - `POST /api/v1/vision`
    - `POST /api/v1/vision/refine`
    - `POST /api/v1/requirements`
    - `PATCH /api/v1/requirements/:id`
    - `DELETE /api/v1/requirements/:id`
    - `POST /api/v1/requirements/draft`
    - `POST /api/v1/requirements/refine`
- `crates/orchestrator-web-api/src/services/web_api_service.rs`
  - add service methods backing these handlers using planning service APIs.
  - parse/validate request payloads deterministically and return stable
    actionable errors for invalid input.

### 5. OpenAPI and docs alignment
- `crates/orchestrator-web-server/openapi.json`
  - register new planning endpoints and request/response contracts.
- Keep server docs behavior unchanged:
  - `/api/v1/openapi.json`
  - `/api/v1/docs`

## Vision Refinement Implementation Constraint
Current AI-assisted vision refinement runtime is implemented under CLI planning
ops. Web API implementation for TASK-013 should choose one explicit path:
1. Extract reusable refinement runtime into `orchestrator-core` and call it from
   web API.
2. Implement a deterministic server-native refine path that preserves output
   shape expected by UI.

Either path must avoid importing `orchestrator-cli` into web crates.

## Suggested Implementation Sequence
1. Add web API methods and web server routes for planning mutations.
2. Update OpenAPI with new endpoints.
3. Add API client methods + decoders + unit tests in web UI.
4. Add planning routes and nav entry.
5. Implement planning screens for vision and requirements flows.
6. Add deep-link and not-found behavior for planning requirement detail.
7. Run test/build validation for UI + touched Rust crates.

## Validation Targets
- `cargo test -p orchestrator-web-api`
- `cargo test -p orchestrator-web-server`
- `npm --prefix crates/orchestrator-web-server/web-ui run test`
- `npm --prefix crates/orchestrator-web-server/web-ui run build`

## Risks and Mitigations
- Risk: endpoint/output drift between UI contracts and server handlers.
  - Mitigation: codify payload guards and add contract tests before screen work.
- Risk: scope creep into non-planning shell behavior.
  - Mitigation: keep routing/navigation edits limited to planning additions.
- Risk: vision refine behavior inconsistency across CLI vs web.
  - Mitigation: settle one reusable refinement path and add regression tests.
