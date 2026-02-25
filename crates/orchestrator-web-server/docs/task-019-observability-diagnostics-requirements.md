# TASK-019 Requirements: Structured Observability and Diagnostics Panel

## Phase
- Workflow phase: `requirements`
- Workflow ID: `2b794d5c-76b0-4933-b1b3-d6886c030684`
- Task: `TASK-019`

## Objective
Define a production-ready client observability layer for the standalone daemon web
UI that provides:
- structured client telemetry for API actions,
- deterministic redaction of sensitive data,
- correlation IDs for request-to-diagnostics traceability,
- an operator-facing diagnostics panel for failed actions.

## Existing Baseline
- Web UI API calls are centralized in
  `crates/orchestrator-web-server/web-ui/src/lib/api/client.ts`.
- Envelope parsing and error normalization are handled by
  `src/lib/api/envelope.ts`.
- Current `ApiError` shape is `{ code, message, exitCode }` with no correlation
  metadata.
- Route-level error rendering exists, but there is no global diagnostics view for
  action failures.
- There is no client redaction utility or telemetry event buffer.

## Scope
In scope for implementation following this requirements phase:
- Add a client telemetry pipeline for API request lifecycle events.
- Define and enforce a redaction policy for telemetry and diagnostics payloads.
- Add correlation ID generation and propagation for API actions.
- Add diagnostics UI for failed actions with actionable, sanitized detail.
- Add tests for redaction, correlation propagation, telemetry capture, and
  diagnostics rendering.

Out of scope for this task:
- Shipping telemetry to external vendors/services.
- Backend schema migrations for `.ao` state files.
- Replacing `ao.cli.v1` response envelope shape.
- Full analytics dashboards beyond the failed-action diagnostics panel.

## Constraints
- Preserve all current `/api/v1` endpoint contracts and `ao.cli.v1` envelope
  semantics.
- Keep behavior deterministic and repository-local (no network dependency for
  telemetry storage).
- Do not log or render raw sensitive fields in telemetry or diagnostics views.
- Preserve existing route shell behavior and API-only server behavior.
- Keep UI keyboard operable and usable at `320px` width without horizontal page
  scrolling.

## Functional Requirements

### FR-01: Telemetry Pipeline
- Instrument `requestAo` as the canonical telemetry entrypoint for HTTP calls.
- Emit structured events for request lifecycle:
  - `request_start`
  - `request_success`
  - `request_failure`
- Capture at minimum:
  - timestamp
  - HTTP method
  - request path
  - action name (when available)
  - duration in milliseconds (for success/failure)
  - HTTP status (if response available)
  - normalized error metadata (`code`, `message`, `exitCode`) on failures
  - correlation ID
- Store events in a client-side ring buffer with deterministic capacity.
- Expose telemetry events through a typed read API for UI components.

### FR-02: Redaction Policy
- Telemetry and diagnostics outputs must run through a shared redaction utility
  before storage or rendering.
- Header handling:
  - Never persist raw values for `authorization`, `cookie`, `set-cookie`, or
    similarly sensitive auth/session headers.
  - Preserve a safe allowlist (`accept`, `content-type`,
    `x-ao-correlation-id`) as plain text.
- Payload key handling (case-insensitive): redact values for keys containing:
  - `token`
  - `secret`
  - `password`
  - `passphrase`
  - `api_key` / `apikey`
  - `credential`
  - `private_key`
  - `session`
- Query string handling: apply the same sensitive-key redaction.
- Redaction should preserve payload structure so diagnostics remain actionable.
- Non-JSON request/response bodies must be represented as a safe placeholder,
  not raw text dumps.

### FR-03: Correlation IDs
- Generate a correlation ID for each action/request chain initiated in the UI.
- Propagate correlation ID via request header `X-AO-Correlation-ID`.
- Include the same correlation ID in telemetry events and normalized API errors.
- If a response includes `X-AO-Correlation-ID`, treat response value as
  canonical for downstream diagnostics for that request.
- Correlation ID must be visible in the diagnostics panel and copyable by users.

### FR-04: Diagnostics Panel for Failed Actions
- Provide a diagnostics panel that surfaces recent failed actions.
- At minimum show for each failure:
  - action label
  - method + path
  - timestamp
  - duration
  - HTTP status (if available)
  - normalized error (`code`, `message`, `exitCode`)
  - correlation ID
  - sanitized request summary
  - sanitized response summary
- Panel behavior:
  - most recent failures first,
  - bounded list size (same ring-buffer style cap),
  - clear empty state when no failures,
  - optional dismiss/clear control for local session data.
- Panel must not require opening browser devtools to access diagnostics context.

### FR-05: Error Model Updates
- Extend client error model to include optional diagnostics metadata:
  - `correlationId`
  - `httpStatus`
  - `requestPath`
  - `method`
- Existing callers that only read `code/message/exitCode` must remain compatible.

### FR-06: Action Coverage
- At minimum, diagnostics must capture failures from current mutating actions:
  - daemon controls (`start`, `pause`, `resume`, `stop`, `clear logs`)
  - review handoff submission
- Implementation must generalize to all future non-GET API actions using the
  shared client.

## Non-Functional Requirements

### NFR-01: Performance and Stability
- Telemetry capture should add negligible UI overhead and must not block request
  completion.
- Bounded in-memory storage must prevent unbounded growth.
- Failures in telemetry processing/redaction must not crash user actions.

### NFR-02: Accessibility
- Diagnostics panel controls must be keyboard operable.
- New failure notifications should be announced politely (`aria-live="polite"`)
  without stealing focus.
- Panel must provide semantic headings and readable structure for screen readers.

### NFR-03: Responsive Behavior
- Desktop: diagnostics panel can render as inline side section or stacked region
  without clipping content.
- Mobile (`<960px`): panel content must remain readable and actionable without
  horizontal scrolling.

## UX and Information Hierarchy Requirements
- Failed-action diagnostics should use strong visual hierarchy:
  - summary (count/state)
  - per-failure headline (action + code)
  - expandable details.
- Keep request/response snippets compact and scannable by default, with
  progressive disclosure for full sanitized payload details.
- Correlation ID should be visually distinct and easy to copy.
- Error messages should prioritize user actionability (what failed, where, and
  what to do next) over raw implementation details.

## Data Contract for Telemetry Events
Required event fields:
- `eventType`: `request_start|request_success|request_failure`
- `timestamp`
- `correlationId`
- `method`
- `path`
- `action`
- `durationMs` (success/failure)
- `httpStatus` (when available)
- `error` object (failure only): `{ code, message, exitCode }`
- `request` (sanitized)
- `response` (sanitized, optional)

## Acceptance Criteria
- `AC-01`: `requestAo` emits telemetry lifecycle events for API calls.
- `AC-02`: Telemetry event storage is bounded and deterministic.
- `AC-03`: Sensitive headers and sensitive payload/query fields are redacted
  before telemetry storage and UI rendering.
- `AC-04`: Every failed mutating action includes correlation ID in diagnostics.
- `AC-05`: Outgoing API requests carry `X-AO-Correlation-ID` header.
- `AC-06`: Normalized client errors can carry correlation and HTTP metadata
  without breaking existing usage.
- `AC-07`: Diagnostics panel renders recent failed actions with required fields.
- `AC-08`: Diagnostics panel supports keyboard-only interaction and readable
  mobile layout.
- `AC-09`: Existing `ao.cli.v1` envelope parsing behavior remains intact.
- `AC-10`: Existing route behavior and current action flows remain functional
  when no failures occur.

## Testable Acceptance Checklist
- `T-01`: Unit test for redaction utility covers headers, JSON bodies, query
  strings, and nested sensitive keys.
- `T-02`: Unit test for correlation ID injection validates outgoing
  `X-AO-Correlation-ID` header.
- `T-03`: Unit test verifies failure telemetry event includes normalized error and
  correlation metadata.
- `T-04`: Unit test verifies ring-buffer cap and eviction behavior.
- `T-05`: Component test verifies diagnostics panel renders failure list and empty
  state correctly.
- `T-06`: Component test verifies keyboard interaction for expand/collapse and
  clear/dismiss controls.
- `T-07`: Regression tests confirm existing envelope parsing and route-level data
  loading behavior remain unchanged.

## Acceptance Verification Matrix
| Requirement | Verification method |
| --- | --- |
| Telemetry lifecycle capture | Unit tests around `requestAo` + telemetry store |
| Redaction enforcement | Unit tests for redaction utility + diagnostics payload snapshots |
| Correlation propagation | Unit tests for request headers and error/diagnostics metadata |
| Failed-action diagnostics UI | React component tests with mocked failures |
| Accessibility baseline | Keyboard interaction assertions and landmark semantics checks |
| No regressions to envelope contract | Existing `envelope`/`client` tests plus targeted regressions |

## Implementation Notes (Input to Next Phase)
Recommended source targets:
- `crates/orchestrator-web-server/web-ui/src/lib/telemetry/`
  - event types
  - ring-buffer store
  - redaction utility
  - correlation ID utility
- `crates/orchestrator-web-server/web-ui/src/lib/api/client.ts`
  - instrumentation hook-in
  - correlation header injection
  - enriched error metadata
- `crates/orchestrator-web-server/web-ui/src/app/`
  - diagnostics panel component
  - page integration points for failed actions
- `crates/orchestrator-web-server/web-ui/src/styles.css`
  - diagnostics panel states and responsive behavior
- `crates/orchestrator-web-server/web-ui/src/lib/**/*.test.ts(x)`
  - unit/component coverage for acceptance checklist

## Deterministic Deliverables for Implementation Phase
- Add typed telemetry primitives and bounded store module.
- Add shared redaction utility with test coverage.
- Add correlation ID generation/propagation path in shared API client.
- Add diagnostics panel UI and wire it to failed action telemetry.
- Add/extend tests for telemetry, redaction, correlation, and diagnostics
  rendering.
