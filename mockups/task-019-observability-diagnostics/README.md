# TASK-019 Wireframes: Structured Observability and Diagnostics Panel

Concrete wireframes for diagnostics UX in the standalone daemon web UI.
These boards focus on failed-action triage, redacted payload visibility,
correlation ID workflow, and responsive accessibility from desktop to `320px`.

## Files
- `wireframes.html`: visual wireframe boards for daemon/review diagnostics states.
- `wireframes.css`: shared style system and responsive/accessibility constraints.
- `diagnostics-wireframe.tsx`: React-oriented component/state skeleton for handoff.

## Route Coverage

| Route | Covered in |
| --- | --- |
| `/daemon` | `wireframes.html` (`Daemon Controls + Diagnostics`) + `diagnostics-wireframe.tsx` (`DaemonDiagnosticsScreen`) |
| `/reviews/handoff` | `wireframes.html` (`Review Handoff + Diagnostics Continuity`) + `diagnostics-wireframe.tsx` (`ReviewHandoffDiagnosticsScreen`) |

## State Coverage
- Action state: `idle`, `pending`, `success`, `failure`
- Diagnostics state: `empty`, `list`, `expanded-detail`, `copy-feedback`, `copy-fallback-manual`, `clear-confirmed`
- Data continuity state: failures visible across both target routes in same local session

## Diagnostics Data Contract Modeled
Each failure row includes:
- action label
- method + path
- timestamp + duration
- HTTP status (when available)
- normalized error (`code`, `message`, `exitCode`)
- correlation ID (copyable)
- sanitized request and response summaries

## Accessibility and Responsive Intent
- Semantic heading and list structure for diagnostics panel.
- Expand controls modeled as buttons with `aria-expanded` + `aria-controls`.
- New failure updates modeled with `aria-live="polite"`.
- Copy controls use explicit labels for screen readers.
- Copy fallback state keeps correlation IDs selectable when clipboard APIs fail.
- Mobile board is explicitly `320px` wide and avoids horizontal page scrolling.
- Touch controls respect 44px minimum target sizing.

## Redaction UX Rules Applied in Mockups
- Sensitive header values are replaced with `[REDACTED]`.
- Sensitive body/query keys retain shape but redact values.
- Non-JSON payload samples are represented by `[NON_JSON_PAYLOAD]`.
- Correlation header remains visible (`X-AO-Correlation-ID`).

## Acceptance Criteria Traceability

| AC | Wireframe trace |
| --- | --- |
| `AC-01` | Request lifecycle status strip and failure timeline boards in `wireframes.html` |
| `AC-02` | Bounded retention (`25 latest`) callout in diagnostics header |
| `AC-03` | Redacted request/response examples in expanded detail panels |
| `AC-04` | Correlation ID chip + copy buttons on each failure row |
| `AC-05` | Header preview includes `X-AO-Correlation-ID` in request summary blocks |
| `AC-06` | Error metadata card includes correlation, method, path, status placeholders |
| `AC-07` | Shared diagnostics panel with empty and populated states |
| `AC-08` | Keyboard/touch annotations, mobile 320px board layout, and manual copy fallback state |
| `AC-09` | Error examples preserve `ao.cli.v1` semantics (`code/message/exitCode`) |
| `AC-10` | Non-failure idle/success messaging kept intact in action area |
