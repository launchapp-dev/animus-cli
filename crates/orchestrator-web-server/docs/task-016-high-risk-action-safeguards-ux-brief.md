# TASK-016 UX Brief: High-Risk Action Safeguards in Web UI

## Phase
- Workflow phase: `ux-research`
- Workflow ID: `9f3fbbad-f13f-4c31-ae02-09398e9e3b36`
- Task: `TASK-016`

## UX Objective
Design a deterministic, operator-safe daemon action experience that prevents
accidental destructive operations while keeping routine daemon controls fast.

The UX must introduce:
- explicit confirmation for high-risk actions,
- side-effect-free pre-submit preview content,
- auditable post-action feedback with correlation context,
- keyboard-first and mobile-safe behavior at `320px` width.

## Primary Users and Jobs

| User | Primary jobs | UX success signal |
| --- | --- | --- |
| Operator | Run daemon lifecycle actions safely under time pressure | High-risk actions are never dispatched without deliberate typed intent |
| On-call engineer | Triage failed daemon actions quickly | Can correlate feedback entry to diagnostics failure record in <= 2 interactions |
| Reviewer / lead | Confirm operational controls are deterministic and auditable | Action history clearly shows what was attempted, when, and outcome |

## UX Principles for This Phase
1. Risk-proportional friction: only high-risk operations get typed confirmation.
2. Intent before mutation: irreversible effects are shown before submit.
3. Deterministic interaction: identical inputs produce identical guardrail behavior.
4. Auditability in context: recent action outcomes remain visible on the daemon page.
5. Accessibility by default: guardrails remain fully keyboard operable and screen-reader legible.

## Information Architecture

### In-Scope Route Surface
- Primary route: `/daemon`
- Shared supporting surface on same route: `DiagnosticsPanel` filtered by `daemon.*`

### Daemon Page Section Order (Post-TASK-016)
1. Page heading and route description.
2. Daemon action controls (`start`, `pause`, `resume`, `stop`, `clear logs`).
3. Guarded action confirmation dialog (conditional, high-risk only).
4. Guarded action feedback timeline (success + failure, bounded).
5. Daemon diagnostics panel (failed details and payload context).
6. Health and logs data panels.

### Risk Classification and UX Treatment

| Action key | Risk | UX treatment |
| --- | --- | --- |
| `daemon.stop` | high | typed confirmation + preview + auditable feedback |
| `daemon.clear_logs` | high | typed confirmation + preview + auditable feedback |
| `daemon.pause` | medium | direct action + auditable feedback (typed confirmation deferred) |
| `daemon.start` | low | direct action + auditable feedback |
| `daemon.resume` | low | direct action + auditable feedback |

## Key Screens and Interaction Contracts

| Screen/surface | Goal | Primary interactions | Required states |
| --- | --- | --- | --- |
| `/daemon` (default) | Show daemon control entrypoint and recent system data | Trigger action buttons, inspect feedback/diagnostics, refresh via post-action data reload | loading, ready, action-pending, action-success, action-failure |
| High-risk confirmation dialog | Prevent accidental destructive dispatch | Open from `Stop`/`Clear Logs`, review impact + preview, type exact intent phrase, confirm/cancel | closed, open-invalid-intent, open-valid-intent, submitting, failed-closed |
| High-risk preview panel (inside dialog) | Expose planned impact before commit | Read method/path, planned effects, irreversible consequences, daemon snapshot, rollback guidance | snapshot-available, snapshot-partial, guidance-present, guidance-unavailable |
| Guarded action feedback list | Provide auditable action outcomes in-session | Read newest-first entries, scan action/outcome/correlation ID, correlate with diagnostics | empty, list-populated, capacity-eviction-active |
| Daemon diagnostics panel | Preserve deep failure troubleshooting path | Expand failure rows, copy correlation ID, inspect sanitized payload details, clear diagnostics | empty, list-ready, detail-expanded, copy-success, copy-fallback |

## Interaction Matrix (Deterministic Contracts)

| Trigger | Guard condition | Immediate UI response | Mutating request allowed |
| --- | --- | --- | --- |
| Click `Stop daemon` | none | Open confirmation dialog with `STOP DAEMON` phrase and preview payload | no |
| Click `Clear daemon logs` | none | Open confirmation dialog with `CLEAR DAEMON LOGS` phrase and preview payload | no |
| Type confirmation phrase | `trim(input) === requiredPhrase` with case-sensitive compare | Enable confirm button only when exact match is true | no |
| Click dialog `Confirm` | valid phrase and no guarded action already pending | Transition to submitting state and lock duplicate submit | yes |
| Click dialog `Cancel` or press `Escape` | dialog open | Close dialog, clear typed state, restore trigger focus | no |
| Click `Start` / `Resume` / `Pause` | no guarded action already pending | Dispatch directly and show pending state for chosen action | yes |
| Request resolves success | request in-flight | Append success feedback row and announce update via live region | n/a |
| Request resolves failure | request in-flight | Append failure feedback row and expose correlation ID for diagnostics linkage | n/a |

## Critical User Flows

### Flow A: High-Risk Stop Daemon (Success)
1. User clicks `Stop` on `/daemon`.
2. UI opens modal dialog instead of dispatching network request.
3. Dialog shows required phrase `STOP DAEMON`, method/path, impact summary, and current daemon snapshot.
4. Confirm button remains disabled until input matches phrase exactly (trim edges only).
5. User confirms; one pending state starts and duplicate submits are blocked.
6. Request completes; feedback record is appended with timestamp, action, outcome, code/message, and correlation ID.
7. Dialog closes, typed input resets, and daemon health/log data refreshes.

### Flow B: High-Risk Clear Logs (Cancel)
1. User clicks `Clear Logs`.
2. Dialog opens with phrase `CLEAR DAEMON LOGS` and irreversible warning.
3. User presses `Escape` or activates `Cancel`.
4. Dialog closes with no network mutation.
5. Focus returns to the original trigger button.

### Flow C: High-Risk Failure and Diagnostics Linkage
1. User confirms high-risk action after valid typed intent.
2. API call fails and returns envelope error.
3. Failure feedback entry appears in action feedback list with correlation ID.
4. Same failure appears in diagnostics panel under `daemon.*`.
5. User expands diagnostics row and validates matching correlation ID for triage.

### Flow D: Low/Medium Risk Direct Execution
1. User clicks `Start`, `Resume`, or `Pause`.
2. Action dispatches immediately (no typed-intent modal in `TASK-016`).
3. Pending state prevents duplicate submissions for active action.
4. Success or failure generates auditable feedback and updates diagnostics for failures.

### Flow E: Mobile Guarded Confirmation (`320px`)
1. User opens high-risk dialog on narrow viewport.
2. Dialog content stacks vertically in logical reading order.
3. Phrase input, preview content, and confirm/cancel controls remain visible without horizontal scroll.
4. Touch targets remain at least `44x44px`.

## Interaction Rules and Guardrail Behavior

### Typed Intent Rules
- `daemon.stop` requires `STOP DAEMON`.
- `daemon.clear_logs` requires `CLEAR DAEMON LOGS`.
- Matching is case-sensitive after trimming leading/trailing whitespace.
- Confirm is disabled until the exact phrase is present.
- Typed input is cleared whenever the dialog closes.

### Pending and Concurrency Rules
- Only one guarded action may be pending at a time in daemon controls.
- Active pending control must show deterministic in-flight text.
- Duplicate clicks for the same action are ignored while pending.
- Correlation ID remains stable from dispatch through feedback rendering.

### Fail-Closed Rules
- Missing or invalid guardrail metadata blocks dispatch.
- High-risk actions with invalid phrase state block dispatch.
- Blocked dispatch shows explicit user-visible error state.

## Requirement Traceability (UX to FR)

| Requirement | UX coverage in this brief |
| --- | --- |
| `FR-01` Guarded action registry | Risk table and interaction matrix define one behavior source for action key, risk, and safeguards |
| `FR-02` Confirmation gate | High-risk dialog contracts, Flow A/B, typed-intent rules, and fail-closed rules |
| `FR-03` Preview/dry-run surface | Dialog screen contract and preview content constraints in Flow A and hierarchy guidance |
| `FR-04` Execution safeguards | Pending/concurrency rules and interaction matrix submit lock behavior |
| `FR-05` Auditable feedback | Feedback list states, information hierarchy, and behavioral constraints (cap 50, ordering, eviction) |
| `FR-06` Diagnostics integration | Flow C and diagnostics panel linkage by shared correlation ID |
| `FR-07` Accessibility requirements | Accessibility section and keyboard/focus behavior in Flow B and mobile flow |
| `FR-08` Responsive hierarchy | Desktop/mobile layout guidance and `320px` constraints |
| `FR-09` Feedback record contract | Feedback hierarchy fields and deterministic ordering/capacity rules |

## Layout, Hierarchy, and Spacing Guidance

### Desktop (`>= 960px`)
- Keep action controls in a compact row near the top of the daemon screen.
- Render feedback and diagnostics as distinct stacked panels below controls.
- In dialog, present hierarchy in this order:
1. Action title + risk label.
2. Irreversible impact statement.
3. Preview metadata and daemon snapshot.
4. Typed-intent input.
5. Confirm/cancel controls.

### Mobile (`< 960px`)
- Stack action controls into wrapped rows or a single column as needed.
- Dialog uses one-column layout with wrapped method/path and correlation strings.
- Feedback entries render as readable cards/rows with wrapped metadata.
- No horizontal page scrolling at `320px` for guarded-action workflows.

### Spacing and Rhythm
- Use consistent spacing scale (`4/8/12/16/24/32px`).
- Preserve strong separation between destructive intent messaging, preview details, irreversible warnings, and final confirmation controls.

## Accessibility Constraints (Non-Negotiable)
1. Confirmation surface exposes `role="dialog"` and `aria-modal="true"`.
2. Dialog has programmatic heading and description IDs tied with `aria-labelledby`/`aria-describedby`.
3. Initial focus moves into dialog on open and returns to triggering button on close.
4. `Escape` dismisses dialog without dispatching mutation.
5. All controls (open, close, confirm, cancel, copy, expand details) are keyboard reachable and operable.
6. Confirm button disabled state is announced correctly and linked to typed-intent validity.
7. Outcome updates (success/failure feedback) are announced via polite live region without stealing focus.
8. Focus indicators remain visible with sufficient contrast on all interactive controls.
9. Text and UI affordances meet WCAG AA contrast targets.
10. At `320px`, no horizontal overflow for confirmation, preview, or feedback surfaces.

## Accessibility Scenario Checklist
1. Keyboard-only user opens `Stop daemon`, tabs through preview and input, types phrase, confirms, and returns focus predictably.
2. Screen reader user hears dialog title, impact text, required phrase hint, and disabled/enabled state transitions for confirm.
3. Keyboard-only user cancels high-risk dialog with `Escape` and no mutation occurs.
4. Low-vision user at `200%` zoom can read feedback metadata and correlation ID without clipped controls.
5. Mobile screen-reader user on `320px` width can reach feedback rows and diagnostics expansion without horizontal scroll.

## Feedback Record Information Hierarchy
Per feedback row, display in this order:
1. Outcome badge (`success` or `failure`).
2. Action key (`daemon.*`).
3. Timestamp.
4. Method + path.
5. Message and normalized code (`ok` for success).
6. Correlation ID.

Behavioral constraints:
- Store capacity is fixed at `50` records per session.
- Newest records render first.
- On overflow, oldest entries are evicted first.
- Filtering is scoped to daemon actions for this page.

## Content Guidance
- Use direct, explicit action verbs: `Stop daemon`, `Clear daemon logs`.
- Irreversible warnings should name concrete impact, not generic danger text.
- Show exact required typed phrase adjacent to the input field.
- For failure messaging, show normalized envelope code and actionable next step (`Retry` or `Inspect diagnostics`).

## Risks and Mitigations

| Risk | Impact | Mitigation |
| --- | --- | --- |
| Over-warning causes confirmation fatigue | Slower routine operations and skipped reading | Limit typed confirmation to high-risk actions only |
| Ambiguous destructive impact text | Incorrect operator assumptions | Standardized impact statements in registry metadata |
| Feedback and diagnostics drift | Harder failure triage | Shared correlation ID and aligned action keys (`daemon.*`) |
| Mobile dialog crowding | Input mistakes or hidden controls | One-column responsive layout and wrapped metadata |

## UX Acceptance Checklist for Build Phase
- High-risk actions open confirmation dialog before any network mutation.
- Typed-intent gate is exact-match and blocks submit until valid.
- Preview content is visible pre-submit and remains side-effect free.
- Only one guarded action can be pending at a time.
- Success and failure outcomes both append feedback entries with correlation IDs.
- Diagnostics remains the canonical failure detail surface and aligns by correlation ID.
- Dialog focus handoff, keyboard dismissal, and focus restoration all work.
- Guardrail surfaces remain usable without horizontal scrolling at `320px`.
- Low/medium risk actions remain fast and do not inherit typed confirmation in this task.
