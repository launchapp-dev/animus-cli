# TASK-019 UX Brief: Structured Observability and Diagnostics Panel

## Phase
- Workflow phase: `ux-research`
- Workflow ID: `2b794d5c-76b0-4933-b1b3-d6886c030684`
- Task: `TASK-019`

## UX Objective
Design a clear, low-friction diagnostics experience that helps operators
understand and recover from failed API actions without opening browser devtools.

The experience must:
- expose recent failures in a bounded, deterministic panel,
- make correlation IDs easy to locate and copy,
- preserve request/response context in sanitized form,
- remain readable and keyboard-operable from `320px` mobile widths to desktop.

## Primary Users and Jobs

| User | Primary jobs | UX success signal |
| --- | --- | --- |
| Operator | Run daemon control actions and troubleshoot failures quickly | Can identify failed endpoint, error code, and correlation ID in under 10 seconds |
| Reviewer | Submit review handoff payloads and diagnose failed submissions | Can recover from failed submit without leaving the page |
| On-call engineer | Triage user-reported UI failures with correlation context | Can copy correlation ID and sanitized request/response details in 1-2 interactions |

## UX Principles for This Phase
1. Error context before implementation detail: show what failed, where, and what to do next first.
2. Safe by default: all diagnostics payloads are redacted before storage and rendering.
3. Progressive disclosure: default views stay compact; details expand on demand.
4. Deterministic behavior: latest failures appear first with bounded retention and explicit empty state.
5. Correlation first: every failed action prominently exposes a copyable correlation ID.

## Information Architecture

### Diagnostics Entry Points
1. `/daemon`
2. `/reviews/handoff`

### Shared Integration Surface
- Reusable diagnostics panel component rendered beneath mutating-action controls.
- Shared failure store (ring-buffer style cap) feeding both pages in local session.
- Optional shell-level summary hook reserved for later phases (not required in TASK-019).

### Diagnostics Panel Structure
1. Panel heading and short purpose text.
2. Summary row: failure count and most recent failure time.
3. Failure list (newest first).
4. Expandable failure detail region per item.
5. Utility actions: copy correlation ID, clear local diagnostics.

## Key Screens and Interaction Contracts

| Screen | Goal | Primary interactions | Required states |
| --- | --- | --- | --- |
| `/daemon` | Execute daemon lifecycle actions with immediate diagnostics fallback | Start/pause/resume/stop/clear logs; expand failed action detail; copy correlation ID; clear diagnostics | idle, action-pending, action-success, action-failure, diagnostics-empty, diagnostics-list |
| `/reviews/handoff` | Submit review handoff with actionable failure context | Submit handoff; expand failure detail; copy correlation ID; clear diagnostics | idle, submitting, submit-success, submit-failure, diagnostics-empty, diagnostics-list |
| Shared diagnostics panel | Provide recent failed-action timeline and sanitized context | Toggle row expansion; keyboard traversal; copy values; clear list | empty, list-ready, expanded-detail, copy-feedback, clear-confirmed |

## Failure Record Information Hierarchy
Per failure row (collapsed):
1. Action label (example: `daemon.stop`, `review.handoff.submit`)
2. Error code + short message
3. Method + path
4. Timestamp and duration

Per failure row (expanded):
1. Correlation ID with copy control
2. HTTP status (when available)
3. Normalized error envelope (`code`, `message`, `exitCode`)
4. Sanitized request summary (headers/query/body preview)
5. Sanitized response summary (headers/body preview when available)

## Critical User Flows

### Flow A: Daemon Action Failure to Recovery
1. User triggers daemon action from `/daemon`.
2. UI starts action-pending feedback and emits `request_start` telemetry.
3. Request fails; UI emits `request_failure` telemetry and shows inline action error.
4. Diagnostics panel prepends a new failure row and announces update politely.
5. User expands row, copies correlation ID, and retries action.

### Flow B: Review Handoff Submission Failure
1. User submits form on `/reviews/handoff`.
2. API request carries correlation header and telemetry capture.
3. Failure appears in submit feedback and diagnostics panel.
4. User inspects sanitized request payload to verify non-sensitive fields.
5. User adjusts input and retries without leaving page context.

### Flow C: Cross-Screen Diagnostics Continuity
1. Failure occurs on `/daemon`.
2. User navigates to `/reviews/handoff`.
3. Panel still shows recent failures from local in-memory buffer.
4. User clears diagnostics explicitly when triage is complete.

### Flow D: Mobile Triage
1. User opens diagnostics on small screen (`<960px`).
2. Each failure appears as a vertical card with summary first.
3. Details expand inline without horizontal scroll.
4. Copy and clear actions remain reachable via touch targets >= `44x44px`.

## Layout, Hierarchy, and Spacing Guidance

### Desktop (`>= 960px`)
- Keep action controls and immediate status near top of route content.
- Render diagnostics panel as a full-width stacked section under primary action region.
- Use clear section spacing so action feedback and diagnostics are visually distinct.

### Mobile (`< 960px`)
- Keep diagnostics panel stacked in document flow.
- Prioritize one-column content; avoid side-by-side metadata rows.
- Keep long values (correlation IDs, paths, JSON previews) wrapped and scroll-safe.

### Visual Rhythm
- Use consistent spacing scale (`4/8/12/16/24/32px`).
- Apply stronger typography contrast for failure headline vs metadata.
- Reserve warning styling for genuine failures; do not over-style empty state.

## Accessibility Constraints (Non-Negotiable)
1. Diagnostics panel has semantic heading and is discoverable by landmarks.
2. Failure list uses semantic list markup; each item has a clear accessible name.
3. Expand/collapse controls are buttons with `aria-expanded` and `aria-controls`.
4. New failure additions are announced via `aria-live="polite"` and do not steal focus.
5. Copy controls have explicit labels (for example, "Copy correlation ID for daemon.stop failure").
6. Keyboard traversal must support summary-to-detail interaction without pointer use.
7. Focus ring remains visible on all actionable controls with sufficient contrast.
8. Text and key UI affordances meet WCAG AA contrast expectations.
9. At `320px`, no horizontal page scrolling for diagnostics workflows.
10. Redacted fields remain readable as structure, with value placeholders such as `[REDACTED]`.

## Content and Redaction UX Rules
1. Never render raw values for sensitive headers/keys listed by requirements.
2. Keep structural keys visible so users can reason about payload shape.
3. Use deterministic placeholders:
   - sensitive values: `[REDACTED]`
   - non-JSON body previews: `[NON_JSON_PAYLOAD]`
4. Truncate oversized non-sensitive strings with explicit ellipsis marker.
5. Show human-friendly timestamp and duration units (ms).

## Interaction Details

| Interaction | Expected behavior | Error prevention/recovery |
| --- | --- | --- |
| Expand failure row | Toggle details without route jump | Preserve scroll position and focus context |
| Copy correlation ID | Copy plain correlation value and show brief confirmation | If copy fails, show inline fallback message with selectable text |
| Clear diagnostics | Remove local failure history from panel | Require explicit click action; no auto-clear on success |
| Retry action | User re-invokes source control/form action | Prior failure remains in history for auditability |

## Risks and Mitigations

| Risk | Impact | Mitigation |
| --- | --- | --- |
| Diagnostics payload feels too dense | Slow triage and missed key fields | Keep compact summary row and progressive detail expansion |
| Redaction hides too much context | Reduced troubleshooting utility | Preserve field structure and explicit placeholder semantics |
| Multiple errors overwhelm users | Important failures get buried | Sort newest first and cap list with deterministic retention |
| Copy actions are unclear on mobile | Slower support handoff | Place copy controls near value and provide concise confirmation text |

## UX Acceptance Checklist for Implementation Phase
- `/daemon` and `/reviews/handoff` both render diagnostics panel near mutating controls.
- Failed actions append deterministic, newest-first records with required metadata.
- Correlation ID is visible and copyable for every failed mutating action.
- Diagnostics details are expandable/collapsible with keyboard-only operation.
- Empty diagnostics state is explicit and non-alarming.
- Redacted request/response previews preserve structure and hide sensitive values.
- At `320px`, diagnostics interactions are usable without horizontal scrolling.
- New failure announcements are polite and non-disruptive to current focus.
