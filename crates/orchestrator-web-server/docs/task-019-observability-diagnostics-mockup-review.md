# TASK-019 Mockup Review: Structured Observability and Diagnostics Panel

## Phase
- Workflow phase: `mockup-review`
- Workflow ID: `2b794d5c-76b0-4933-b1b3-d6886c030684`
- Task: `TASK-019`

## Scope of Review
Reviewed `TASK-019` wireframe artifacts against:
- `task-019-observability-diagnostics-requirements.md`
- `task-019-observability-diagnostics-ux-brief.md`

Reviewed artifacts:
- `mockups/task-019-observability-diagnostics/wireframes.html`
- `mockups/task-019-observability-diagnostics/wireframes.css`
- `mockups/task-019-observability-diagnostics/diagnostics-wireframe.tsx`
- `mockups/task-019-observability-diagnostics/README.md`

## Mismatch Resolution Log

| Mismatch | Requirement/UX reference | Resolution |
| --- | --- | --- |
| Diagnostics panels were visually clear but lacked explicit landmark helper context for screen-reader navigation | NFR-02 accessibility + UX accessibility rules 1-4 | Added `role="region"` + `aria-describedby` helper copy on desktop/mobile panel variants in `wireframes.html` |
| Review-handoff diagnostics board did not explicitly show retention bound or polite update announcements | FR-04 bounded list + NFR-02 live-region behavior | Added retention callout and `aria-live="polite"` update sample for handoff diagnostics in `wireframes.html` |
| Expanded failure detail did not consistently expose timestamp/duration/http-status/request tuple in a single metadata block | FR-04 required failure fields + UX hierarchy requirements | Added `Request Metadata` card and structured `dl` rows for daemon and handoff expanded failures in `wireframes.html`; mirrored same structure in `diagnostics-wireframe.tsx` |
| Copy-correlation interaction only showed success feedback and had no explicit fallback state | UX interaction details: copy fallback requirement | Added manual copy fallback messaging states in desktop/mobile wireframes and React wireframe skeleton (`wireframes.html`, `diagnostics-wireframe.tsx`, `README.md`) |
| Mobile expanded failure state under-modeled diagnostics detail (missing timestamp and full sanitized context) | NFR-03 responsive behavior + AC-08 | Expanded mobile board detail content to include timestamp plus metadata/error/request/response summary examples while preserving one-column readability (`wireframes.html`) |
| React wireframe component used static heading IDs and only one route demonstrated bounded newest-first behavior | AC-02 bounded deterministic retention + accessibility semantics | Added deterministic newest-first bounded record derivation for both route screens and unique panel IDs via `useId` in `diagnostics-wireframe.tsx` |

## Acceptance Criteria Traceability (Mockup Phase)

| AC | Evidence |
| --- | --- |
| `AC-01` | Lifecycle states and failure timeline callouts remain explicit in daemon/review boards (`wireframes.html`) |
| `AC-02` | Bounded retention (`newest 25`) shown in diagnostics summaries and wireframe state model (`wireframes.html`, `diagnostics-wireframe.tsx`, `README.md`) |
| `AC-03` | Sanitized request/response examples preserve structure with `[REDACTED]` and `[NON_JSON_PAYLOAD]` placeholders (`wireframes.html`, `diagnostics-wireframe.tsx`) |
| `AC-04` | Correlation IDs are shown per failure with copy controls and canonical response correlation context (`wireframes.html`, `diagnostics-wireframe.tsx`) |
| `AC-05` | Request samples include `x-ao-correlation-id` in headers (`wireframes.html`, `diagnostics-wireframe.tsx`) |
| `AC-06` | Error model examples include correlation/status/method/path metadata without dropping `code/message/exitCode` (`wireframes.html`, `diagnostics-wireframe.tsx`) |
| `AC-07` | Diagnostics panel covers empty and populated states with expandable details (`wireframes.html`, `diagnostics-wireframe.tsx`) |
| `AC-08` | Keyboard-friendly toggles, live-region announcements, mobile 320px board, and manual copy fallback state are represented (`wireframes.html`, `wireframes.css`, `README.md`) |
| `AC-09` | Response payload examples keep `ao.cli.v1` envelope semantics unchanged (`wireframes.html`, `diagnostics-wireframe.tsx`) |
| `AC-10` | Action surfaces preserve non-failure route context and retry affordances while diagnostics remain additive (`wireframes.html`) |

## Outcome
`TASK-019` mockups now include explicit requirement-to-artifact traceability and resolve phase-level usability/accessibility mismatches before build implementation.
