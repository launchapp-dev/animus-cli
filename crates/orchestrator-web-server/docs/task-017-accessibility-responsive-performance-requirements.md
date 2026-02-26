# TASK-017 Requirements: Accessibility, Responsive, and Performance Baselines

## Phase
- Workflow phase: `requirements`
- Workflow ID: `d4b62fa3-9d50-474e-810c-28f0cc0b87df`
- Task: `TASK-017`
- Project root: `/Users/samishukri/ao-cli`

## Objective
Define production-ready baseline requirements for the AO web UI so that primary
workflows remain keyboard operable, semantically accessible, responsive across
common viewport classes, and performant under realistic project data sizes.

## Phase Clarification (Current Run)
- This `requirements` phase is documentation-first and does not change runtime
  behavior directly.
- Deliverables in this phase are deterministic requirement/implementation notes
  that unblock the build phase with concrete file targets and validation gates.
- `.ao` state remains unchanged by manual edits; this phase updates repository
  docs only.

## Existing Baseline
- Current shell, route tree, and page screens are implemented under
  `crates/orchestrator-web-server/web-ui/src/app/`.
- Shared API fetch/envelope handling is centralized in
  `src/lib/api/client.ts`, `src/lib/api/envelope.ts`, and
  `src/lib/api/use-api-resource.ts`.
- A diagnostics panel with sanitized failure details exists from `TASK-019`
  (`src/app/diagnostics-panel.tsx`).
- Styling and responsive behavior is currently driven by `src/styles.css` with
  a single main breakpoint (`<= 960px`).
- Accessibility and responsive checks are present only as light baseline tests;
  there is no explicit route performance budget enforcement.

## Scope
In scope for implementation following this requirements phase:
- Keyboard navigation and focus-management hardening across shell/navigation and
  primary actions.
- Semantic accessibility improvements for landmarks, headings, status messaging,
  and form validation feedback.
- Responsive layout hardening for mobile, tablet, and desktop widths, including
  overflow and dense data readability.
- Deterministic core route performance budgets and repository-local validation.
- Targeted tests for accessibility behavior, responsive guardrails, and
  performance budget compliance.

Out of scope for this task:
- Redesign of route information architecture introduced in `TASK-011`.
- Backend API contract/schema changes for `/api/v1`.
- Third-party telemetry/performance vendor integration.
- Broad visual redesign beyond baseline hierarchy/clarity/readability upgrades.

## Constraints
- Preserve existing `/api/v1` endpoint contracts and `ao.cli.v1` envelope
  semantics.
- Preserve route coverage and navigation targets defined by `APP_ROUTE_PATHS`.
- Keep behavior deterministic and repository-local (no dependence on external
  runtime services for measurements).
- Preserve diagnostics telemetry contracts introduced in `TASK-019`.
- Keep `.ao` state mutation out of manual file edits.
- Maintain usability at `320px` width without horizontal page scrolling.
- Keep implementation aligned with current route-loading strategy (`lazy` route
  exports + suspense fallback in `router.tsx`).

## Functional Requirements

### FR-01: Keyboard Navigation and Focus Management
- Add a skip link at shell level to jump directly to `#main-content`.
- Mobile navigation must be fully keyboard operable:
  - menu toggle button opens/closes drawer,
  - `Escape` closes the drawer when open,
  - focus moves into drawer on open and returns to menu button on close.
- All primary action buttons on route screens must be reachable and activatable
  via keyboard only.
- Focus indicators must remain visible on all interactive elements.

### FR-02: Semantic Landmarks, Headings, and Status Messaging
- Preserve clear landmark structure: shell-level `header`, `nav`, and `main`,
  with route content sections clearly labeled by heading IDs.
- Route sections must expose deterministic heading hierarchy and labeling for
  screen-reader navigation.
- Loading and empty-state messages should use semantic status treatment
  (`role="status"` + polite live behavior) where applicable.
- Error states should keep assertive semantics (`role="alert"`).

### FR-03: Accessible Form Validation and Messaging
- Review handoff form validation errors must be associated to relevant fields
  (`aria-invalid` + `aria-describedby` or equivalent semantic association).
- Validation copy must remain specific and actionable (what failed and how to
  fix it).
- Required-field constraints should be represented semantically, not only
  visually.

### FR-04: Responsive Layout and Density Baseline
- Support three viewport classes:
  - mobile: `320px..599px`
  - tablet: `600px..959px`
  - desktop: `>=960px`
- No critical control may become hidden/unreachable at mobile widths.
- Dense data blocks (`pre`, diagnostics metadata, events feed) must avoid page
  horizontal overflow and preserve readability.
- Navigation, project context controls, and route actions must remain usable
  under one-column mobile layout.

### FR-05: Consistent Route State Hierarchy
- Loading, empty, ready, and error states must stay consistent across route
  families via shared rendering semantics.
- Route sections should keep predictable content order:
  - heading and context description,
  - actions/controls,
  - stateful content panels.

### FR-06: Core Route Performance Budgets
- Define and enforce repository-local performance budgets for active web bundle
  artifacts referenced by `embedded/index.html`.
- Budget thresholds:
  - referenced JS entry asset (gzip): `<= 110 KiB`
  - referenced CSS entry asset (gzip): `<= 8 KiB`
- Core route data fans (`dashboard`, `project detail`, `workflow detail`) must
  keep parallel fetch behavior (no regression to serial request chains).
- Rendered lists for continuously updating or failure-heavy views must remain
  bounded:
  - daemon events UI cap remains bounded (`25` shown, `200` stored),
  - diagnostics failures remain bounded by configured capacity.

## Non-Functional Requirements

### NFR-01: Accessibility Baseline
- Primary flows satisfy keyboard-only operation requirements with visible focus.
- Semantic cues are sufficient for assistive technology users to understand
  page structure and state changes without visual inference.

### NFR-02: Responsive Baseline
- Primary workflows remain complete and usable across mobile, tablet, and
  desktop classes.
- Layout transitions avoid clipped controls and horizontal viewport scrolling.

### NFR-03: Performance Stability
- Bundle budgets remain within threshold in CI/local checks.
- Existing bounded data rendering behavior remains enforced to prevent
  unbounded memory/render growth in session-heavy usage.

## UX and Information Hierarchy Requirements
- Keep section headings concise and descriptive by route.
- Place actions close to relevant data panels and feedback.
- Keep status and error copy explicit and scannable.
- Maintain spacing and grouping so controls, feedback, and content are visually
  separable on both mobile and desktop.

## Acceptance Criteria
- `AC-01`: Shell includes a keyboard-reachable skip link targeting
  `#main-content`.
- `AC-02`: Mobile navigation supports keyboard open/close with `Escape` close
  and deterministic focus return.
- `AC-03`: Route content landmarks/headings provide consistent semantic labeling
  for assistive tech navigation.
- `AC-04`: Loading/empty states expose status semantics; error states remain
  alert semantics.
- `AC-05`: Review handoff validation errors are field-associated with semantic
  invalid/description attributes.
- `AC-06`: At `320px` width, primary navigation and route actions remain usable
  with no horizontal page scroll.
- `AC-07`: Tablet and desktop layouts preserve readable hierarchy and action
  discoverability.
- `AC-08`: Dense diagnostics/event metadata remains readable without clipping.
- `AC-09`: Referenced build artifacts meet budgets (`JS gzip <= 110 KiB`,
  `CSS gzip <= 8 KiB`).
- `AC-10`: Route aggregate fetches that are currently parallel remain parallel.
- `AC-11`: Event and diagnostics lists remain bounded to configured capacities.
- `AC-12`: Existing route/API behavior remains compatible with current
  `TASK-011` and `TASK-019` contracts.
- `AC-13`: Implementation phase includes deterministic repository-local
  validation commands for accessibility/responsive/performance checks.

## Testable Acceptance Checklist
- `T-01`: Component test verifies skip-link presence and target focus behavior.
- `T-02`: Component test verifies keyboard menu behavior (`open`, `Escape`
  close, focus return).
- `T-03`: Component test verifies landmark and heading associations for shell
  and route sections.
- `T-04`: Component test verifies loading/empty/error semantic roles in shared
  state renderers.
- `T-05`: Component test verifies review handoff field-level validation
  semantics.
- `T-06`: Responsive regression checks at `320`, `768`, and `1280` widths
  confirm no horizontal viewport overflow for key routes.
- `T-07`: Deterministic performance budget check validates gzip size of
  referenced JS/CSS assets in `embedded/index.html`.
- `T-08`: Unit test verifies dashboard/project/workflow aggregate requests
  remain parallelized (`Promise.all` path).
- `T-09`: Unit test verifies daemon events and diagnostics failure buffers remain
  bounded.
- `T-10`: Existing API/envelope/telemetry tests continue to pass without
  behavior regressions.

## Implementation Validation Gates (Build Phase)
- `V-01`: `npm run test -- src/app/accessibility-responsive-baselines.test.ts`
  passes (shell landmarks, focus hooks, responsive breakpoint guards).
- `V-02`: `npm run test -- src/app/build-performance.test.ts` passes (chunking
  and warning-threshold guardrails).
- `V-03`: `npm run build` succeeds for `web-ui`.
- `V-04`: performance budget script (to be added in build phase) succeeds
  against `crates/orchestrator-web-server/embedded/index.html` with:
  - JS gzip `<= 110 KiB`
  - CSS gzip `<= 8 KiB`
- `V-05`: full `npm run test` remains green to protect route/API/telemetry
  compatibility.

## Acceptance Verification Matrix
| Requirement | Verification method |
| --- | --- |
| Keyboard navigation and focus management | `shell` component tests for skip link and mobile drawer keyboard flow |
| Semantic landmarks and statuses | Component tests for heading/landmark labels and status/alert roles |
| Form validation accessibility | Component tests for review handoff field error association |
| Responsive usability | Width-specific regression checks for key routes and controls |
| Bundle and route performance baseline | Build-budget checker + bounded-list + parallel-fetch tests |
| Contract compatibility | Existing route/API client/telemetry tests |

## Implementation Notes (Next Phase Input)
- Preferred integration points:
  - shell and navigation: `src/app/shell.tsx`
  - route state rendering and form semantics: `src/app/screens.tsx`
  - responsive + focus visuals: `src/styles.css`
  - event bounds: `src/lib/events/use-daemon-events.ts`
  - diagnostics bounds: `src/lib/telemetry/store.ts`,
    `src/app/diagnostics-panel.tsx`
- Add deterministic performance budget validation as a repository script tied to
  `web-ui` build output.
- Keep deliverables compatible with existing embedded static serving flow in
  `crates/orchestrator-web-server/embedded/`.
- Prefer extending existing baseline tests before creating new test files:
  `src/app/accessibility-responsive-baselines.test.ts`,
  `src/app/build-performance.test.ts`, and relevant screen-level tests.

## Deterministic Deliverables for Implementation Phase
- Accessibility hardening updates in shell/screens/styles and related tests.
- Responsive refinements covering mobile, tablet, and desktop baseline behavior.
- Performance budget checker for referenced build artifacts.
- Updated tests proving keyboard accessibility, semantic state messaging,
  responsive guardrails, and budget compliance.
