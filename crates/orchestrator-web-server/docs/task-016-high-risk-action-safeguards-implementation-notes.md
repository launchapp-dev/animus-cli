# TASK-016 Implementation Notes: High-Risk Action Safeguards in Web UI

## Phase Context
- Workflow phase: `requirements`
- Workflow ID: `9f3fbbad-f13f-4c31-ae02-09398e9e3b36`
- Task: `TASK-016`

## Purpose
Translate `TASK-016` requirements into deterministic implementation slices for
the build phase, focused on high-risk daemon actions in the web UI.

## Non-Negotiable Constraints
- Keep all API calls under `/api/v1`.
- Preserve `ao.cli.v1` envelope parsing and existing endpoint contracts.
- Do not manually edit `.ao` state files.
- Keep safeguards client-local and deterministic (no external storage).
- Preserve existing telemetry redaction behavior from `TASK-019`.
- Keep controls keyboard operable and usable at `320px` width.

## Baseline Integration Points
- Daemon action controls:
  `crates/orchestrator-web-server/web-ui/src/app/screens.tsx`
- Existing failed-action diagnostics:
  `crates/orchestrator-web-server/web-ui/src/app/diagnostics-panel.tsx`
- API transport and action naming:
  `crates/orchestrator-web-server/web-ui/src/lib/api/client.ts`
- Telemetry event capture and ring buffer:
  `crates/orchestrator-web-server/web-ui/src/lib/telemetry/store.ts`
- UI styling:
  `crates/orchestrator-web-server/web-ui/src/styles.css`

## Current Implementation Snapshot (Before Build Phase)
- High-risk controls (`stop`, `clear`) already open an inline safeguard section.
- Typed-intent behavior is currently weaker than requirement contract:
  - phrase set: `STOP` / `CLEAR LOGS`
  - matcher: case-insensitive with internal whitespace normalization.
- The current dry-run mode creates local preview entries but does not produce
  the final normalized feedback schema required for auditability.
- Feedback rendering is currently an unbounded JSON dump and does not guarantee
  bounded capacity (`50`) or complete correlation-aware fields.
- Concurrency and pending behavior are partially implemented but not yet locked
  to explicit single in-flight guardrail invariants.

## Locked Build Decisions from Requirements Phase
- Typed-intent phrases are exactly:
  - `STOP DAEMON` for `daemon.stop`
  - `CLEAR DAEMON LOGS` for `daemon.clear_logs`
- Phrase matching is trim-only and case-sensitive.
- High-risk confirmation is modal (`role="dialog"`, `aria-modal="true"`) with
  focus handoff/restoration.
- High-risk preview content is shown pre-submit and remains side-effect free.
- `daemon.pause`, `daemon.start`, and `daemon.resume` remain direct execution
  actions (no typed intent) for `TASK-016`.
- Feedback history is bounded to `50`, evicts oldest-first, and renders
  newest-first.

## Proposed Source Layout
- `crates/orchestrator-web-server/web-ui/src/app/daemon-action-guards.ts`
  - typed action registry with risk classification and preview metadata
  - typed-intent phrases for high-risk actions
- `crates/orchestrator-web-server/web-ui/src/app/daemon-confirmation-dialog.tsx`
  - modal confirmation surface for high-risk actions
  - typed phrase input + preview rendering + keyboard handling
- `crates/orchestrator-web-server/web-ui/src/app/daemon-action-feedback.tsx`
  - success/failure action feedback list (bounded, most-recent-first)
- `crates/orchestrator-web-server/web-ui/src/app/screens.tsx`
  - replace inline safeguard panel with modal guarded flow orchestration
  - use bounded feedback view instead of raw JSON action dump
- `crates/orchestrator-web-server/web-ui/src/app/*.test.tsx`
  - confirmation, pending-state, feedback, and accessibility coverage

## Action Policy Contract
Action keys and required safeguards:
- `daemon.stop`: high risk, typed confirmation phrase `STOP DAEMON`,
  preview required
- `daemon.clear_logs`: high risk, typed confirmation phrase
  `CLEAR DAEMON LOGS`, preview required
- `daemon.pause`: medium risk, no typed confirmation in `TASK-016`
- `daemon.start`: low risk
- `daemon.resume`: low risk

Implementation rule:
- Only high-risk actions are blocked by modal confirmation in this task.
- Medium/low actions continue direct execution path with auditable feedback.

## Guardrail State Machine (UI-Local)
Suggested state model for daemon controls:
- `idle`
- `confirming` (with selected action + required phrase + preview snapshot)
- `submitting` (single pending action key + stable correlation ID)
- `completed` (feedback record appended, state returns to `idle`)
- `failed_closed` (guardrail invariant violation blocks dispatch with clear UI
  error)

Fail-closed checks before dispatch:
- Selected action exists in registry.
- Action risk class is known.
- For high-risk actions, typed phrase check passes.
- No conflicting pending action exists.

## Preview/Dry-Run Notes
Preview must be fully client-side and deterministic:
- show action metadata from registry (`method`, `path`, impact summary)
- show latest daemon status/health snapshot already loaded in the page
- show irreversible consequences for `daemon.stop` and `daemon.clear_logs`
- show rollback guidance when available

No preview path may perform a mutating network request.
Preview is informational and rendered before confirmation submit.

## Feedback and Diagnostics Notes
Action feedback record fields:
- `id`
- `timestamp`
- `action`
- `method`
- `path`
- `outcome`
- `message`
- `code`
- `correlationId`

Feedback behavior:
- bounded to `50` records in-memory with oldest-first eviction
- render most-recent-first
- include a polite live region announcement for new entries
- maintain correlation consistency with diagnostics (`DiagnosticsPanel`)

Recommended linkage:
- derive feedback entries from `daemon.*` telemetry events so success and
  failure records share the same correlation source as diagnostics.

## Accessibility and Responsive Notes
Confirmation dialog requirements:
- `role="dialog"` and `aria-modal="true"`
- focus moved into dialog on open and restored to triggering button on close
- `Escape` closes dialog without dispatch
- visible focus indicators for all dialog controls

Layout requirements:
- desktop: preview + confirmation controls grouped and scannable
- mobile: stacked layout with no horizontal scrolling at `320px`
- irreversible impact text must remain visually prominent

## Suggested Build Sequence
1. Add typed daemon action registry and phrase/preview metadata.
2. Add confirmation dialog component with preview rendering and focus handling.
3. Refactor daemon action orchestration in `screens.tsx` to use guard state.
4. Add bounded feedback component and wire it under daemon controls.
5. Keep existing diagnostics panel; verify correlation alignment.
6. Add styles for dialog/preview/feedback across desktop and mobile.
7. Add/update component and store tests.
8. Run web UI tests and fix regressions.

## Testing Targets
- `src/app/daemon-confirmation-dialog.test.tsx`
  - high-risk actions open modal, not immediate dispatch
  - typed-phrase gating rules (case-sensitive, trim-only)
  - cancel and `Escape` close without mutation
- `src/app/screens.test.tsx`
  - pending state prevents duplicate submissions
  - high-risk flow dispatches only after valid confirmation
- `src/app/daemon-action-feedback.test.tsx`
  - success/failure record shape and render ordering
  - capacity cap (`50`) and deterministic eviction
- Accessibility checks:
  - dialog semantics, focus handoff/restoration, keyboard operability
  - no horizontal overflow for confirmation/feedback at `320px`

## Deferred Follow-Ups (Not in TASK-016)
- Persisting action feedback across browser reloads.
- Server-side dry-run/preflight endpoints.
- Typed confirmation for medium-risk actions (`daemon.pause`).
