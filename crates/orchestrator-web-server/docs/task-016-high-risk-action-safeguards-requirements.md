# TASK-016 Requirements: High-Risk Action Safeguards in Web UI

## Phase
- Workflow phase: `requirements`
- Workflow ID: `9f3fbbad-f13f-4c31-ae02-09398e9e3b36`
- Task: `TASK-016`

## Objective
Define deterministic, production-safe safeguards for destructive or
service-impacting actions in the standalone daemon web UI by adding:
- explicit confirmation gates,
- preview/dry-run surfaces before commit,
- auditable action feedback after execution.

## Existing Baseline
- Daemon mutating controls and safeguard flow are implemented in
  `crates/orchestrator-web-server/web-ui/src/app/screens.tsx`.
- Daemon mutating actions currently exposed in UI:
  - `daemon.start`
  - `daemon.pause`
  - `daemon.resume`
  - `daemon.stop`
  - `daemon.clear_logs`
- High-risk actions currently open an inline safeguard panel (not a modal
  dialog) with typed input and a local dry-run checkbox.
- Current typed phrases and matching logic are not aligned to target policy:
  - phrase set is `STOP` / `CLEAR LOGS`
  - matching is case-insensitive and normalizes internal whitespace.
- Current action feedback is a local JSON dump (`ActionRecord[]`) and does not
  enforce bounded capacity or complete correlation-aware schema requirements.
- Existing failures are surfaced in `DiagnosticsPanel`, but successful actions
  are not yet rendered through a normalized auditable record surface.

## Baseline Gaps to Close in Build Phase
- Upgrade high-risk confirmation surface from inline section to accessible modal
  dialog semantics.
- Enforce deterministic typed-intent contract (`STOP DAEMON`,
  `CLEAR DAEMON LOGS`) with trim-only matching.
- Replace ad-hoc action record rendering with bounded feedback entries that
  include correlation-aware fields and deterministic ordering.
- Enforce single in-flight guarded action behavior and deterministic pending
  state handling.

## Scope
In scope for implementation after this phase:
- Add a high-risk action guardrail system for daemon destructive operations.
- Add explicit confirmation UX for high-risk actions before network mutation.
- Add a deterministic preview/dry-run panel shown before confirmation.
- Add action feedback records (success and failure) with correlation IDs for
  operator auditability.
- Add accessibility and responsive behavior requirements for confirmation and
  preview flows.
- Add tests that cover confirmation gating, preview rendering, and action
  feedback behavior.

Out of scope for this task:
- Server-side dry-run endpoint additions or backend contract changes.
- New daemon mutation APIs beyond existing `/api/v1/daemon/*` endpoints.
- Persisting action audit history across browser reloads.
- Replacing or removing existing diagnostics telemetry pipeline from `TASK-019`.

## Constraints
- Preserve existing `/api/v1` endpoint contracts and `ao.cli.v1` envelope
  semantics.
- Do not perform direct manual edits to `.ao` state files.
- Keep safeguards deterministic and client-local (no external telemetry storage).
- Keep redaction policy intact for all displayed request/response details.
- Keep behavior keyboard operable and usable at `320px` width.
- Keep destructive safeguards scoped to high-risk actions only; do not introduce
  confirmation fatigue for low-risk read or navigation operations.

## High-Risk Action Inventory and Classification

| Action | Endpoint | Risk level | Safeguard requirement |
| --- | --- | --- | --- |
| `daemon.stop` | `POST /api/v1/daemon/stop` | high | full confirmation + preview + auditable feedback |
| `daemon.clear_logs` | `DELETE /api/v1/daemon/logs` | high | full confirmation + preview + auditable feedback |
| `daemon.pause` | `POST /api/v1/daemon/pause` | medium | direct execution + auditable feedback |
| `daemon.start` | `POST /api/v1/daemon/start` | low | auditable feedback only |
| `daemon.resume` | `POST /api/v1/daemon/resume` | low | auditable feedback only |

Notes:
- This task must fully enforce safeguards for high-risk actions.
- Medium/low entries are included for deterministic policy behavior and
  predictable future extension, but typed confirmation is required only for
  high-risk actions.
- Medium-risk confirmation behavior is explicitly deferred to a follow-up task;
  `TASK-016` must not block `daemon.pause` with typed confirmation.

## Deterministic Typed-Intent Contract

| Action | Required phrase |
| --- | --- |
| `daemon.stop` | `STOP DAEMON` |
| `daemon.clear_logs` | `CLEAR DAEMON LOGS` |

Rules:
- Phrase matching is case-sensitive after trimming leading/trailing whitespace.
- Internal whitespace is not normalized; input must match expected phrase
  exactly after edge-trim.
- Submit stays disabled until the input matches the required phrase exactly.
- The required phrase must be shown inline in the confirmation UI adjacent to
  the input control.

## Functional Requirements

### FR-01: Guarded Action Registry
- Define a typed registry mapping UI action keys to:
  - risk level,
  - endpoint metadata (`method`, `path`),
  - destructive flag,
  - preview text/planned effects.
- Registry must be the single source used by action buttons, preview, and
  confirmation flows to avoid drift.

### FR-02: Confirmation Gate for High-Risk Actions
- High-risk actions must not execute immediately on first click.
- First click opens a confirmation surface that includes:
  - action name,
  - impact statement,
  - endpoint/method metadata,
  - planned effects list.
- For high-risk actions, confirmation requires explicit typed intent phrase.
- Confirm button remains disabled until required phrase matches exactly.
- Cancel closes confirmation without mutating state.
- Confirmation state must be reset when the dialog closes so stale typed input
  cannot carry across different high-risk actions.

### FR-03: Preview/Dry-Run Surface (Client-Side)
- Before submit, UI must render a deterministic preflight preview:
  - current daemon state snapshot (latest known status/health),
  - planned effects list,
  - irreversible consequences (if any),
  - rollback guidance (if available).
- Preview must be side-effect free (no mutating network call).
- Preview must be tied to the specific action and not reusable across different
  actions.
- Preview content is required for high-risk actions before confirm; an optional
  preview-only trigger is allowed but not required.

### FR-04: Execution Safeguards
- Only one guarded action execution may be pending at a time per page section.
- While an action is pending:
  - disable duplicate submit controls for that action,
  - show pending state with deterministic text,
  - keep the chosen correlation ID stable for request/feedback linkage.
- On completion, pending state is cleared deterministically.

### FR-05: Auditable Action Feedback
- Add visible feedback records for both successful and failed guarded actions.
- Feedback record must include:
  - timestamp,
  - action key,
  - method + path,
  - outcome (`success` or `failure`),
  - normalized message/code,
  - correlation ID.
- Feedback must be filterable to daemon action scope and bounded in-memory.
- Feedback capacity is fixed at `50` records per browser session; when capacity
  is exceeded, oldest records are evicted first.
- Feedback list must be ordered most-recent-first.

### FR-09: Action Feedback Record Contract
- Feedback record schema:
  - `id` (UI-local stable identifier),
  - `timestamp` (ISO-8601),
  - `action` (`daemon.*` key),
  - `method`,
  - `path`,
  - `outcome` (`success|failure`),
  - `message`,
  - `code` (or `"ok"` for success entries),
  - `correlationId`.
- Success records may be projected from `request_success` telemetry events and
  failure records may be projected from `request_failure` telemetry events to
  preserve correlation consistency.

### FR-06: Diagnostics Integration
- Existing `DiagnosticsPanel` remains the canonical failed-action detail surface.
- Guarded-action feedback must link conceptually to diagnostics data through
  shared correlation ID.
- Failures from guarded actions must continue to appear in diagnostics with
  sanitized payloads.

### FR-07: Accessibility Requirements
- Confirmation surface must:
  - expose `role="dialog"` + `aria-modal="true"` semantics,
  - move initial focus into dialog on open,
  - support keyboard activation and dismissal (`Escape` + explicit cancel),
  - restore focus to triggering control on close.
- Feedback updates must announce outcome in a polite live region without
  stealing focus.
- All confirmation and preview controls must be keyboard operable.

### FR-08: Responsive and Visual Hierarchy Requirements
- Desktop:
  - keep confirmation and preview content visually grouped and scannable.
- Mobile (`<960px`):
  - confirmation, preview, and feedback layouts must stack cleanly with no
    horizontal page scrolling.
- Spacing and hierarchy must emphasize:
  - action intent,
  - irreversible effects,
  - primary confirmation/cancel decision.

## Non-Functional Requirements

### NFR-01: Determinism
- Confirmation phrase rules, preview content, and feedback ordering must be
  deterministic across runs for the same inputs.

### NFR-02: Safety
- No high-risk action request can be dispatched without passing confirmation
  gate checks.
- UI-side safeguards must fail closed (if guardrail state is invalid, execution
  is blocked and clear error shown).

### NFR-03: Performance and Stability
- Guardrail logic must not materially delay request dispatch after confirmation.
- Feedback store must be bounded to avoid unbounded memory growth.
- Guardrail/feedback rendering failures must not crash page-level navigation.

## Acceptance Criteria
- `AC-01`: High-risk actions (`daemon.stop`, `daemon.clear_logs`) require explicit
  confirmation before request dispatch.
- `AC-02`: High-risk confirmation requires deterministic typed intent and blocks
  submit until valid.
- `AC-02a`: Typed-intent phrases are exactly:
  - `STOP DAEMON` for `daemon.stop`
  - `CLEAR DAEMON LOGS` for `daemon.clear_logs`
- `AC-03`: A pre-submit dry-run/preview surface is rendered for high-risk
  actions and remains side-effect free.
- `AC-04`: Pending action state prevents duplicate submissions while in flight.
- `AC-05`: Every guarded action completion emits visible action feedback with
  correlation ID.
- `AC-06`: Guarded-action failures remain visible in diagnostics with sanitized
  payloads.
- `AC-07`: Existing `/api/v1` endpoint usage and `ao.cli.v1` parsing behavior
  remain unchanged.
- `AC-08`: Confirmation and feedback flows pass keyboard-only interaction checks.
- `AC-09`: At `320px` width, confirmation and feedback surfaces remain usable
  without horizontal page scroll.
- `AC-10`: Existing low-risk route data loading and navigation behavior remain
  functionally unchanged.
- `AC-11`: Action feedback history is bounded to `50` entries with deterministic
  oldest-first eviction and most-recent-first rendering.
- `AC-12`: `daemon.pause`, `daemon.start`, and `daemon.resume` remain direct
  execution actions in `TASK-016` (no typed-intent requirement).

## Testable Acceptance Checklist
- `T-01`: Component test verifies high-risk action click opens confirmation
  dialog instead of dispatching network request.
- `T-02`: Component test verifies typed confirmation gating enables submit only
  after exact intent phrase.
- `T-02a`: Component test verifies high-risk phrase matching is case-sensitive
  and trims leading/trailing whitespace only.
- `T-03`: Component test verifies cancel path closes dialog with no mutation.
- `T-04`: Component/integration test verifies preview panel shows planned
  effects and current daemon snapshot data.
- `T-05`: Component test verifies pending state disables duplicate submit for
  guarded action.
- `T-06`: Component test verifies success feedback entry includes correlation ID,
  method/path, and timestamp.
- `T-07`: Component test verifies failure feedback entry and diagnostics linkage
  by correlation ID.
- `T-08`: Accessibility test verifies dialog semantics, focus handoff, keyboard
  dismissal, and focus restoration.
- `T-09`: Responsive test verifies no horizontal overflow in guardrail surfaces
  at `320px` viewport width.
- `T-10`: Regression tests verify existing API client envelope parsing and daemon
  page data rendering remain intact.
- `T-11`: Store/component test verifies feedback capacity limit (`50`) and
  deterministic eviction order.
- `T-12`: Component test verifies medium/low risk daemon actions do not require
  typed-intent confirmation.

## Acceptance Verification Matrix
| Requirement | Verification method |
| --- | --- |
| Confirmation gating for high-risk actions | React component tests around daemon controls and action dispatch mocks |
| Side-effect-free preview | Component tests asserting no network call until explicit confirm |
| Pending and duplicate-submit protection | Interaction test with repeated click attempts during in-flight request |
| Auditable feedback records | Store/component tests for record shape, ordering, and bounded capacity |
| Diagnostics continuity | Existing diagnostics tests + targeted correlation linkage assertions |
| Accessibility and responsive baseline | Keyboard/focus tests + viewport layout assertions |
| No contract regressions | Existing `client`/`envelope` tests and daemon route smoke tests |

## Implementation Notes (Input to Next Phase)
Primary source targets:
- `crates/orchestrator-web-server/web-ui/src/app/screens.tsx`
  - replace current inline safeguard section with modal guarded flow
- `crates/orchestrator-web-server/web-ui/src/app/`
  - add confirmation/preview component(s)
  - add action feedback panel component(s)
- `crates/orchestrator-web-server/web-ui/src/lib/telemetry/`
  - add/read helpers for action feedback projections from telemetry events
- `crates/orchestrator-web-server/web-ui/src/lib/api/client.ts`
  - ensure guarded actions preserve stable `actionName` and correlation linkage
- `crates/orchestrator-web-server/web-ui/src/styles.css`
  - add responsive, accessible styling for modal/preview/feedback states
- `crates/orchestrator-web-server/web-ui/src/app/*.test.tsx`
  - add tests for confirmation, preview, pending safeguards, and feedback

## Deterministic Deliverables for Implementation Phase
- Add a typed high-risk action registry and guardrail state machine for daemon
  actions.
- Add confirmation dialog with typed intent for high-risk operations.
- Add side-effect-free preview/dry-run surface before submit.
- Add bounded action feedback surface with correlation-aware success/failure
  records.
- Add tests covering guardrail behavior, accessibility, responsiveness, and
  regressions.

Companion implementation notes:
- `crates/orchestrator-web-server/docs/task-016-high-risk-action-safeguards-implementation-notes.md`
