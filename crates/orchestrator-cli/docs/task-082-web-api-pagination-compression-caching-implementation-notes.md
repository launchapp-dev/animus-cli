# TASK-082 Implementation Notes: Web API Pagination, Compression, and Conditional Caching

## Phase Context
- Workflow phase: `requirements`
- Workflow ID: `d5334375-a0f6-4914-af94-5de3709f9a9d`
- Task: `TASK-082`

## Purpose
Translate TASK-082 requirements into an implementation slice that reduces web
API polling cost and payload size without destabilizing non-target endpoints.

## Non-Negotiable Constraints
- Keep changes focused on targeted list/caching/compression surfaces.
- Preserve AO envelope semantics and non-target endpoint behavior.
- Keep cursor behavior deterministic and versioned.
- Do not manually edit `.ao/*.json`.

## Chosen Strategy
- Add pagination at the web API boundary for target list endpoints so service
  internals remain untouched in this slice.
- Add compression in the Axum web server via `tower-http` middleware.
- Add conditional response handling in web-server route handlers for the two
  frequently polled endpoints.
- Propagate page-size defaults/limits through `web serve` CLI args to
  `WebServerConfig`.
- Update OpenAPI + web-ui API contracts to keep consumers aligned.

This approach addresses the performance bottleneck while containing risk to a
small, testable set of files.

## Proposed Change Surface

### 1) Pagination Query and Response Modeling
Targets:
- `crates/orchestrator-web-server/src/services/web_server.rs`
- `crates/orchestrator-web-api/src/services/web_api_service/requests.rs`
- `crates/orchestrator-web-api/src/services/web_api_service/{tasks_handlers.rs,workflows_handlers.rs,requirements_handlers.rs}`

Actions:
- add shared pagination query model (`cursor`, `page_size`) for targeted
  handlers.
- introduce paginated response shape:
  - `items`
  - `page_info` (`next_cursor`, `has_more`, `page_size`, `returned`, `total`)
- apply paging only to:
  - `/api/v1/tasks`
  - `/api/v1/workflows`
  - `/api/v1/requirements`

### 2) Deterministic Cursor Helpers
Targets:
- `crates/orchestrator-web-api/src/services/web_api_service/mod.rs` (or a new helper module under that tree)

Actions:
- add cursor encode/decode helpers with version tag (`v1`).
- define deterministic validation errors for malformed/unsupported cursors.
- keep implementation simple and stable (opaque cursor over positional index).

### 3) Configurable Page-Size Limits
Targets:
- `crates/orchestrator-web-server/src/models/web_server_config.rs`
- `crates/orchestrator-cli/src/cli_types/web_types.rs`
- `crates/orchestrator-cli/src/services/operations/ops_web.rs`

Actions:
- extend `WebServerConfig` with page-size default/max fields.
- add CLI args to `ao web serve` for page-size default/max controls.
- validate configuration invariants at startup (`default >= 1`, `max >= default`).

### 4) Compression Middleware
Targets:
- `crates/orchestrator-web-server/Cargo.toml`
- `crates/orchestrator-web-server/src/services/web_server.rs`

Actions:
- add `tower-http` dependency with compression features.
- apply `CompressionLayer` (gzip + br) on API router.
- ensure SSE route remains uncompressed (route layering or predicate).

### 5) ETag Conditional Responses
Targets:
- `crates/orchestrator-web-server/src/services/web_server.rs`

Actions:
- add helper to serialize selected payloads and compute deterministic weak ETag.
- wire `If-None-Match` handling for:
  - `daemon_status_handler`
  - `tasks_list_handler`
- on match, return `304 Not Modified` with ETag header and empty body.

### 6) OpenAPI Contract Updates
Target:
- `crates/orchestrator-web-server/openapi.json`

Actions:
- add `cursor`/`page_size` query parameters for targeted list endpoints.
- document paginated response data shape and conditional headers for affected
  routes.

### 7) Web UI Contract Alignment
Targets:
- `crates/orchestrator-web-server/web-ui/src/lib/api/client.ts`
- `crates/orchestrator-web-server/web-ui/src/lib/api/contracts/guards.ts`
- related tests in `web-ui/src/lib/api/client.test.ts` and guard tests

Actions:
- update list decoders to read paginated payload shape.
- keep API helpers deterministic and avoid silent decode failures.

### 8) Tests
Primary targets:
- `crates/orchestrator-web-server/src/services/web_server.rs` test module
- web-ui API client/guard tests for payload contract

Recommended coverage:
- cursor parse/encode and malformed cursor path
- pagination metadata correctness (`next_cursor`, `has_more`, `returned`)
- default page size `50` + max clamp
- ETag `200` then `304` behavior
- `Accept-Encoding` compression header assertions
- SSE regression (still stream-capable)

## Suggested Implementation Sequence
1. Add config + CLI argument wiring for page-size defaults/max.
2. Add pagination query structs and helper functions.
3. Implement paginated payloads for target list handlers.
4. Add ETag helper + conditional logic in daemon status and tasks list routes.
5. Add compression middleware and SSE safeguard.
6. Update OpenAPI for params/response contract.
7. Update web-ui decoders/client tests for paginated payloads.
8. Run targeted tests and fix regressions.

## Validation Targets
- `cargo test -p orchestrator-web-api`
- `cargo test -p orchestrator-web-server`
- targeted web-ui API contract tests for changed decoders

## Risks and Mitigations
- Risk: payload contract break for existing list consumers.
  - Mitigation: update web-ui decoders in the same change and keep tests aligned.
- Risk: compression interferes with SSE buffering.
  - Mitigation: explicitly exempt `text/event-stream`.
- Risk: unstable ETag values from non-canonical serialization.
  - Mitigation: generate ETag from stable serialized payload bytes only.
- Risk: cursor drift when backing list changes between requests.
  - Mitigation: document cursor as best-effort snapshot cursor for current list
    order and keep deterministic slicing per request.

## Deliverables for Next Phase
- Targeted list endpoints emit paginated payloads with cursor metadata.
- Compression middleware reduces response size for eligible endpoints.
- `If-None-Match` on daemon status/task list can return `304`.
- Page-size defaults/limits are configurable and validated.
- OpenAPI and web-ui contract tests reflect the new behavior.
