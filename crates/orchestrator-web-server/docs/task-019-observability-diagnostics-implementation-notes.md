# TASK-019 Implementation Notes: Structured Observability and Diagnostics Panel

## Purpose
Translate `TASK-019` requirements into deterministic implementation slices for
the build phase, while preserving existing API and routing behavior.

## Non-Negotiable Constraints
- Keep all API calls under `/api/v1`.
- Preserve `ao.cli.v1` envelope parsing behavior.
- Do not manually edit `.ao` state files.
- Keep telemetry storage local and bounded (no external telemetry sink).
- Never render or persist raw sensitive fields in diagnostics payloads.

## Baseline Integration Points
- API transport + normalization:
  `crates/orchestrator-web-server/web-ui/src/lib/api/client.ts`
- Envelope types:
  `crates/orchestrator-web-server/web-ui/src/lib/api/envelope.ts`
- Route screens with mutating actions:
  `crates/orchestrator-web-server/web-ui/src/app/screens.tsx`
- Existing styles:
  `crates/orchestrator-web-server/web-ui/src/styles.css`

## Proposed Source Layout
- `crates/orchestrator-web-server/web-ui/src/lib/telemetry/types.ts`
  - telemetry event types and diagnostics record types
- `crates/orchestrator-web-server/web-ui/src/lib/telemetry/store.ts`
  - bounded in-memory ring buffer + subscribe/get APIs
- `crates/orchestrator-web-server/web-ui/src/lib/telemetry/redaction.ts`
  - deterministic redaction helpers for headers/body/query
- `crates/orchestrator-web-server/web-ui/src/lib/telemetry/correlation.ts`
  - correlation ID generator/normalizer
- `crates/orchestrator-web-server/web-ui/src/lib/telemetry/index.ts`
  - small facade exports
- `crates/orchestrator-web-server/web-ui/src/app/diagnostics-panel.tsx`
  - UI for failed actions and sanitized detail

## API Client Refactor Notes
1. Extend request options in `requestAo` to accept optional `actionName` and
   `correlationId` overrides.
2. Generate correlation ID when not provided.
3. Inject `X-AO-Correlation-ID` into request headers.
4. Emit `request_start` telemetry with sanitized request metadata.
5. On response:
   - parse envelope exactly as today,
   - collect status and optional response correlation header,
   - emit `request_success` or `request_failure` telemetry.
6. On network/JSON/payload/decode failure:
   - keep existing error code mapping,
   - enrich `ApiError` with correlation + HTTP metadata,
   - emit `request_failure` telemetry with sanitized summaries.

## Redaction Strategy Notes
Use one shared utility to avoid policy drift.

Redaction precedence:
1. Header redaction first.
2. Query key redaction.
3. Body redaction (JSON object traversal).
4. Fallback placeholder for non-JSON payloads.

Implementation rules:
- case-insensitive key matching,
- preserve object/array shape,
- replace sensitive values with `[REDACTED]`,
- enforce max preview length for long strings,
- avoid throwing on unexpected input types.

## Diagnostics Panel Notes
- Render as a dedicated panel component near mutating action controls (daemon
  page and review handoff page), with option to elevate to shell-level later.
- UI states:
  - empty (no failures)
  - list (recent failures)
  - detail expanded per failure
- Include copy action for correlation ID and sanitized payload block.
- Add clear-history control for local session diagnostics.
- Keep panel keyboard and screen-reader friendly.

## Suggested Build Sequence
1. Add telemetry types/store utilities + tests.
2. Add redaction utility + tests.
3. Add correlation ID utility + tests.
4. Integrate telemetry + correlation into `requestAo`.
5. Extend `ApiError` type for diagnostics metadata.
6. Add diagnostics panel component and style hooks.
7. Wire diagnostics panel to daemon and review pages.
8. Add/update tests for API client failures and panel behavior.
9. Run web UI tests and build; fix regressions before finalizing.

## Testing Targets
- `src/lib/api/client.test.ts`
  - correlation header presence
  - telemetry failure event content
  - compatibility with existing endpoint tests
- `src/lib/telemetry/*.test.ts`
  - redaction policy coverage
  - ring buffer cap behavior
- `src/app/*.test.tsx`
  - diagnostics panel rendering
  - keyboard interaction and clear behavior

## Regression Guardrails
- Keep `ApiError` backward compatible for existing consumers.
- Ensure route-level `ResourceStateView` behavior is unchanged for successful
  loads.
- Do not alter SSE event stream contract in this task.
- Preserve visual hierarchy and spacing consistency with existing shell styles.

## Deferred Follow-Ups (Not in TASK-019)
- Persisting diagnostics across page reloads.
- Server-side correlation/trace linkage beyond header propagation.
- Cross-session telemetry export and aggregation.
