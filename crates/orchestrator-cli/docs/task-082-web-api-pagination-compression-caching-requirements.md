# TASK-082 Requirements: Web API Pagination, Compression, and Conditional Caching

## Phase
- Workflow phase: `requirements`
- Workflow ID: `d5334375-a0f6-4914-af94-5de3709f9a9d`
- Task: `TASK-082`
- Snapshot date: `2026-02-27`

## Objective
Reduce payload size and polling overhead on the standalone daemon web API by
adding:
- cursor-based pagination on collection read endpoints for tasks, workflows,
  and requirements,
- gzip/brotli response compression at the web server boundary,
- ETag conditional GET behavior for high-frequency polling endpoints,
- configurable page-size limits with a default page size of `50`.

The implementation must remain deterministic, Rust-only, and safe for existing
AO repository workflows.

## Current Baseline Audit

| Surface | Current location | Current behavior | Gap |
| --- | --- | --- | --- |
| Route wiring for list endpoints | `crates/orchestrator-web-server/src/services/web_server.rs` | `/api/v1/tasks`, `/api/v1/workflows`, `/api/v1/requirements` return unbounded list payloads | no `cursor`/`page_size` query contract |
| Web API list handlers | `crates/orchestrator-web-api/src/services/web_api_service/{tasks_handlers,workflows_handlers,requirements_handlers}.rs` | handlers return full vectors directly from service APIs | no pagination wrapper, no page metadata |
| Polling endpoints | `web_server.rs` + `daemon_handlers.rs` | `/api/v1/daemon/status` and `/api/v1/tasks` always return full `200` JSON bodies | no `ETag`/`If-None-Match` support |
| Server middleware | `crates/orchestrator-web-server/Cargo.toml`, `web_server.rs` | no `tower-http` compression layer applied | no automatic gzip/brotli compression |
| Web server runtime config | `crates/orchestrator-web-server/src/models/web_server_config.rs` | host/port/assets/api-only only | no configurable page-size defaults/clamps |
| CLI web serve args | `crates/orchestrator-cli/src/cli_types/web_types.rs` | no pagination sizing flags | no operator-level page-size control |
| OpenAPI contract | `crates/orchestrator-web-server/openapi.json` | list routes have no cursor/page-size parameters or pagination schema | contract does not describe new behavior |
| UI client decoders | `crates/orchestrator-web-server/web-ui/src/lib/api/{client.ts,contracts/guards.ts}` | list decoders currently assume array payloads | incompatible with paginated response object unless adapted |

## Scope
In scope for implementation after this requirements phase:
- Add cursor pagination to collection endpoints:
  - `GET /api/v1/tasks`
  - `GET /api/v1/workflows`
  - `GET /api/v1/requirements`
- Add configurable page-size default/maximum with default page size `50`.
- Add response compression (`gzip`, `br`) using `tower_http::compression::CompressionLayer`.
- Add ETag conditional response handling for:
  - `GET /api/v1/daemon/status`
  - `GET /api/v1/tasks`
- Update OpenAPI and implementation docs to reflect new query parameters and
  response contracts.
- Add targeted tests for pagination correctness, compression behavior, and
  conditional caching.

Out of scope for this task slice:
- Pagination for non-targeted list-like routes (`/tasks/prioritized`,
  `/workflows/{id}/decisions`, `/workflows/{id}/checkpoints`, project-scoped
  aggregate endpoints).
- Streaming/push caching strategy changes for `/api/v1/events`.
- Manual edits to `.ao/*.json`.

## Constraints
- Determinism:
  - cursor encoding/decoding must be deterministic and versioned.
  - identical inputs and backing data produce identical page boundaries.
- Safety:
  - `page_size` must always be clamped to configured bounds.
  - malformed cursors must fail with stable `invalid_input` error semantics.
- Compatibility:
  - keep AO CLI envelope shape (`ao.cli.v1`) unchanged.
  - keep non-target endpoints behavior unchanged.
- Transport correctness:
  - SSE endpoint must remain uncompressed/stream-safe.
  - conditional GET must return `304` with no JSON envelope body.
- Repository policy:
  - Rust crate changes only; no direct `.ao` state mutations.

## API Contract Decisions

### Pagination Query Contract
Target endpoints accept optional query parameters:
- `cursor`: opaque cursor string; absent means first page.
- `page_size`: requested page size.

Normalization:
- default page size: `50`
- minimum page size: `1`
- maximum page size: configurable (default `200`)

### Cursor Contract
- Cursor format is opaque to clients and versioned internally (for example
  base64url-encoded `v1:<offset>`).
- Invalid or unsupported cursor format returns `400` envelope error
  (`invalid_input`).

### Paginated Data Contract (inside `data`)
Collection endpoints return:
- `items`: array of resource items for current page
- `page_info`:
  - `next_cursor: string | null`
  - `has_more: bool`
  - `page_size: number`
  - `returned: number`
  - `total: number`

### Conditional Caching Contract
For `/api/v1/daemon/status` and `/api/v1/tasks`:
- successful `200` responses include:
  - `ETag`
  - `Cache-Control: private, must-revalidate`
- matching `If-None-Match` request returns `304 Not Modified` with no JSON body.
- ETag generation is deterministic from canonical response payload bytes.

### Compression Contract
- Apply `CompressionLayer` to API/static responses when client sends
  `Accept-Encoding`.
- Supported encodings: brotli and gzip.
- SSE (`text/event-stream`) remains uncompressed.
- Response headers include correct `Vary` behavior for encoding negotiation.

## Functional Requirements

### FR-01: Cursor Pagination on Target Collection Endpoints
Add `cursor` and `page_size` handling to `/tasks`, `/workflows`, and
`/requirements` list endpoints and return paginated `data` payloads with
`items` + `page_info`.

### FR-02: Deterministic Cursor and Paging Semantics
Define deterministic cursor encode/decode and page slicing semantics, including
stable error handling for malformed cursors.

### FR-03: Configurable Page-Size Limits
Introduce runtime configuration fields (and CLI plumbing) for page-size default
and maximum, with defaults anchored to `50` and safe clamping behavior.

### FR-04: Response Compression Middleware
Integrate `tower-http` compression middleware in the web server with gzip and
brotli enabled while preserving SSE correctness.

### FR-05: ETag Conditional GET for Polling Endpoints
Add ETag generation and `If-None-Match` processing for daemon status and task
list endpoints with standards-compliant `304` behavior.

### FR-06: OpenAPI and Contract Documentation Updates
Update OpenAPI route parameter definitions and response documentation for new
pagination fields and conditional headers on affected endpoints.

### FR-07: Consumer Compatibility Guard
Ensure web-ui API decoders are updated or made backward-compatible so list
endpoint contract changes do not produce runtime decode failures.

### FR-08: Validation Coverage
Add focused tests covering pagination, page-size clamping, cursor errors, ETag
`304` behavior, and compression negotiation on representative endpoints.

## Acceptance Criteria
- `AC-01`: `/tasks`, `/workflows`, and `/requirements` accept `cursor` and
  `page_size`, and return paginated list payloads.
- `AC-02`: default `page_size` is `50` when unspecified.
- `AC-03`: page-size values are clamped to configured min/max bounds.
- `AC-04`: malformed cursors return deterministic `400 invalid_input`.
- `AC-05`: `CompressionLayer` serves gzip/brotli for eligible responses.
- `AC-06`: `/api/v1/events` SSE behavior remains functional.
- `AC-07`: `/daemon/status` and `/tasks` emit ETags on `200` and return `304`
  on matching `If-None-Match`.
- `AC-08`: OpenAPI reflects pagination query params and affected response
  contract details.
- `AC-09`: web-ui decoding remains valid for affected list routes.
- `AC-10`: targeted checks/tests for touched crates pass.

## Testable Acceptance Checklist
- `T-01`: unit tests for cursor encode/decode and invalid cursor handling.
- `T-02`: endpoint tests for first/next page semantics and `next_cursor`.
- `T-03`: tests for `page_size` default (`50`) and max clamping.
- `T-04`: endpoint tests for ETag emission and `304` no-body behavior on
  daemon status and tasks list.
- `T-05`: integration tests for `Accept-Encoding: gzip` / `br` with
  `Content-Encoding` assertions.
- `T-06`: SSE endpoint regression test proving stream remains reachable.
- `T-07`: web-ui API decoder tests for paginated list payloads.

## Verification Matrix

| Requirement area | Verification method |
| --- | --- |
| FR-01, FR-02 | web-server route tests + web-api pagination helper tests |
| FR-03 | CLI/web-server config tests + normalization tests |
| FR-04 | middleware integration tests asserting `Content-Encoding` |
| FR-05 | route tests for `ETag` and `If-None-Match` |
| FR-06 | OpenAPI diff/review + docs check |
| FR-07 | web-ui `client.test.ts` / guards tests for list decoders |
| FR-08 | targeted crate test runs on touched surfaces |

## Implementation Notes Input (Next Phase)
Primary code targets:
- `crates/orchestrator-web-server/src/services/web_server.rs`
- `crates/orchestrator-web-server/src/models/web_server_config.rs`
- `crates/orchestrator-web-server/Cargo.toml`
- `crates/orchestrator-web-server/openapi.json`
- `crates/orchestrator-web-api/src/services/web_api_service/{tasks_handlers.rs,workflows_handlers.rs,requirements_handlers.rs,mod.rs,requests.rs}`
- `crates/orchestrator-cli/src/cli_types/web_types.rs`
- `crates/orchestrator-cli/src/services/operations/ops_web.rs`
- `crates/orchestrator-web-server/web-ui/src/lib/api/{client.ts,contracts/guards.ts}`

Suggested validation targets:
- `cargo test -p orchestrator-web-server`
- `cargo test -p orchestrator-web-api`
- `cargo test -p orchestrator-cli services::operations::ops_web` (if new args/config wiring includes tests)
- web-ui API client/guard tests for updated payload decoding

## Deterministic Deliverables for Implementation Phase
- Cursor-paginated task/workflow/requirement list endpoints with explicit page
  metadata.
- Configurable page-size defaults/limits with default page size `50`.
- gzip/brotli compression for eligible responses without SSE regression.
- ETag conditional responses for daemon status and task list polling endpoints.
- OpenAPI and consumer contract updates with targeted regression coverage.
