# TASK-016 Wireframes: High-Risk Action Safeguards

Concrete wireframes for safeguard UX on the standalone daemon web UI route
`/daemon`. These mockups model deterministic confirmation gates, side-effect-free
preview panels, and auditable feedback records for destructive actions.

## Files
- `wireframes.html`: desktop + mobile wireframe boards covering guarded actions.
- `wireframes.css`: shared visual system for hierarchy, spacing, and responsive states.
- `daemon-action-safeguards-wireframe.tsx`: React-oriented state and component skeleton.

## Route Coverage

| Route | Covered in |
| --- | --- |
| `/daemon` | `wireframes.html` (`Guarded Actions`, `Confirmation Dialog`, `Mobile 320px`) + `daemon-action-safeguards-wireframe.tsx` (`DaemonActionSafeguardsWireframe`) |

## Action Risk Contract Modeled

| Action key | Method + path | Risk | Mocked UX behavior |
| --- | --- | --- | --- |
| `daemon.start` | `POST /api/v1/daemon/start` | low | direct dispatch + feedback |
| `daemon.pause` | `POST /api/v1/daemon/pause` | medium | direct dispatch + feedback |
| `daemon.resume` | `POST /api/v1/daemon/resume` | low | direct dispatch + feedback |
| `daemon.stop` | `POST /api/v1/daemon/stop` | high | typed confirmation + preview + feedback |
| `daemon.clear_logs` | `DELETE /api/v1/daemon/logs` | high | typed confirmation + preview + feedback |

## Typed-Intent Phrases
- `daemon.stop`: `STOP DAEMON`
- `daemon.clear_logs`: `CLEAR DAEMON LOGS`

Rules represented in the mockups:
- Phrase match is case-sensitive after trim.
- Confirm stays disabled until phrase is exact.
- Dialog close clears typed input and restores trigger focus.

## State Coverage
- Guard state: `idle`, `confirming-invalid`, `confirming-valid`, `submitting`, `failed-closed`
- Feedback state: `empty`, `populated`, `eviction-active` (cap `50`, newest-first, oldest evicted first)
- Feedback scope: `daemon.*` filter active by default with explicit scope toggle control
- Accessibility state: dialog focus handoff, escape/cancel dismissal, polite live updates

## Acceptance Criteria Traceability

| AC | Wireframe trace |
| --- | --- |
| `AC-01` | High-risk actions route to dialog in `wireframes.html` and `onRequestAction` logic in `.tsx` |
| `AC-02` | Typed-intent gating shown in dialog invalid/valid states and `isTypedIntentValid` helper |
| `AC-02a` | Required phrases rendered inline in dialog and encoded in guard registry |
| `AC-03` | Preview panel includes action metadata, snapshot, effects, irreversible consequences, rollback guidance |
| `AC-04` | Submitting state disables duplicate actions and locks one pending action key |
| `AC-05` | Feedback list rows include record ID, actor, timestamp, action, method/path, outcome, message/code, correlation ID |
| `AC-06` | Failure board shows correlation continuity between feedback row and diagnostics panel |
| `AC-07` | Existing `/api/v1` request metadata is shown in preview and feedback without endpoint contract changes |
| `AC-08` | Dialog role/ARIA mapping, focus return, keyboard dismissal, and live-region behavior are represented |
| `AC-09` | Mobile `320px` board shows stacked confirmation + feedback without horizontal overflow |
| `AC-10` | Mockups keep low-risk data/load route behavior unchanged and scoped to daemon guardrail surfaces |
| `AC-11` | Retention cap (`50`) and explicit oldest-first eviction behavior are shown in feedback panel copy |
| `AC-12` | `daemon.pause`, `daemon.start`, and `daemon.resume` remain direct-execution actions in action row and policy notes |

## Linked Requirement Coverage

REQ-016 acceptance criteria is also represented directly:
- Typed confirmation / high-friction acknowledgement is modeled for high-risk actions.
- Side-effect-free preview is shown before submit in dialog boards.
- Auditable context includes actor, timestamp, target action, and outcome.
- Precondition panel explicitly models UI preflight checks and server revalidation before success copy.
- Bulk-operation criterion is captured as a deterministic extension pattern in fail-closed guidance; current daemon actions are single-action mutations in this task scope.

## Accessibility and Responsive Intent
- Dialog semantics: `role="dialog"`, `aria-modal="true"`, label + description IDs.
- Outcome announcements: `aria-live="polite"` region for feedback updates.
- Touch and keyboard: controls keep 44px minimum target size.
- Responsive requirement: dedicated `320px` board demonstrates no horizontal overflow.
