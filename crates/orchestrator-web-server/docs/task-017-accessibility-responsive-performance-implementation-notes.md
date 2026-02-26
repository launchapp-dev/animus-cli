# TASK-017 Implementation Notes: Accessibility, Responsive, and Performance Baselines

## Phase Context
- Workflow phase: `requirements`
- Workflow ID: `d4b62fa3-9d50-474e-810c-28f0cc0b87df`
- Task: `TASK-017`
- Project root: `/Users/samishukri/ao-cli`

## Purpose
Translate `TASK-017` requirements into deterministic implementation slices for
the build phase while preserving existing route and API behavior.

## Non-Negotiable Constraints
- Keep `/api/v1` contracts and `ao.cli.v1` envelope behavior unchanged.
- Do not manually edit `.ao` state files.
- Preserve route topology and navigation targets from `TASK-011`.
- Preserve telemetry/diagnostics behavior from `TASK-019`.
- Keep layout usable at `320px` without horizontal page scrolling.

## Baseline Integration Points
- Shell/navigation and top-level landmarks:
  `crates/orchestrator-web-server/web-ui/src/app/shell.tsx`
- Route sections, state rendering, and review form:
  `crates/orchestrator-web-server/web-ui/src/app/screens.tsx`
- Shared visual system and breakpoints:
  `crates/orchestrator-web-server/web-ui/src/styles.css`
- Bounded live event behavior:
  `crates/orchestrator-web-server/web-ui/src/lib/events/use-daemon-events.ts`
- Bounded diagnostics behavior:
  `crates/orchestrator-web-server/web-ui/src/lib/telemetry/store.ts`,
  `crates/orchestrator-web-server/web-ui/src/app/diagnostics-panel.tsx`
- Existing UI tests:
  `src/app/*.test.ts(x)`, `src/lib/**/*.test.ts`

## Proposed Source Layout Additions
- Prefer extending existing baseline files before adding new test files:
  - `crates/orchestrator-web-server/web-ui/src/app/accessibility-responsive-baselines.test.ts`
    - skip link, drawer keyboard flow, focus-return, breakpoint guardrails
  - `crates/orchestrator-web-server/web-ui/src/app/shell.test.ts`
    - route-nav topology and stable keyboard target identifiers
  - `crates/orchestrator-web-server/web-ui/src/app/build-performance.test.ts`
    - bundling/chunking guardrails plus size-budget wiring assertions
- Optional targeted additions if baseline files become overloaded:
  - `crates/orchestrator-web-server/web-ui/src/app/review-handoff-accessibility.test.tsx`
    - field-level validation semantics and message association
- `crates/orchestrator-web-server/web-ui/scripts/check-performance-budgets.mjs`
  - parse `embedded/index.html`, resolve referenced JS/CSS, enforce gzip budgets

## Accessibility Implementation Notes
1. Shell-level keyboard flow (`shell.tsx`)
- Add a skip link before shell chrome that targets `#main-content`.
- Convert mobile drawer behavior into explicit keyboard lifecycle:
  - open -> focus first nav link,
  - `Escape` closes drawer,
  - close -> focus returns to menu button.
- Ensure menu overlay interaction does not strand keyboard users.

2. Semantic states and headings (`screens.tsx`)
- Use heading IDs + `aria-labelledby` for route sections (instead of generic
  unlabeled groupings).
- Promote shared state render semantics:
  - loading/empty as status regions (`role="status"`),
  - errors retain `role="alert"`.
- Ensure panel/list structures stay semantically meaningful when content updates.

3. Review handoff form semantics (`screens.tsx`)
- Provide field-level validation state and helper/error association:
  - `aria-invalid` on invalid controls,
  - `aria-describedby` linking to deterministic helper/error IDs.
- Keep existing payload validation rules and API action shape unchanged.

## Responsive Implementation Notes
- Extend CSS breakpoints to explicitly handle:
  - mobile (`320..599`),
  - tablet (`600..959`),
  - desktop (`>=960`).
- Harden dense content containers (`pre`, metadata rows, badges, action rows) to
  avoid viewport overflow and clipping.
- Keep one-column mobile readability with consistent spacing and tap-target
  accessibility.

## Performance Baseline Notes
1. Bundle budgets
- Add a repository-local checker script that:
  - reads `crates/orchestrator-web-server/embedded/index.html`,
  - resolves currently referenced JS/CSS assets,
  - calculates gzip byte size,
  - fails if thresholds are exceeded:
    - JS: `<= 110 KiB`
    - CSS: `<= 8 KiB`

2. Route efficiency guardrails
- Keep aggregate route data requests parallel (`Promise.all` for dashboard,
  project detail, workflow detail).
- Preserve bounded collections:
  - events stored cap (`200`) and rendered cap (`25`),
  - diagnostics list bounded by configured capacity.

## Suggested Build Sequence
1. Implement shell keyboard/focus lifecycle improvements.
2. Implement route semantic status/heading updates.
3. Implement review form validation accessibility improvements.
4. Implement responsive CSS refinements across viewport classes.
5. Add/update accessibility component tests.
6. Add deterministic performance budget script + test wiring.
7. Run `npm run test` and `npm run build` in web-ui; fix regressions before
   finalizing.

## Acceptance-to-Implementation Mapping
| Acceptance criteria | Primary files |
| --- | --- |
| `AC-01`, `AC-02` | `src/app/shell.tsx`, `src/styles.css`, `src/app/accessibility-responsive-baselines.test.ts` |
| `AC-03`, `AC-04`, `AC-05` | `src/app/screens.tsx`, `src/styles.css`, route-level tests in `src/app/*.test.ts(x)` |
| `AC-06`, `AC-07`, `AC-08` | `src/styles.css`, `src/app/screens.tsx`, responsive/accessibility baseline tests |
| `AC-09`, `AC-10` | `scripts/check-performance-budgets.mjs`, `src/app/build-performance.test.ts`, `src/app/router.tsx`, aggregate route loaders in `src/app/screens.tsx` |
| `AC-11`, `AC-12` | `src/lib/events/use-daemon-events.ts`, `src/lib/telemetry/store.ts`, existing API/telemetry regression tests |

## Build-Phase Validation Commands
Run in `crates/orchestrator-web-server/web-ui`:
1. `npm run test -- src/app/accessibility-responsive-baselines.test.ts`
2. `npm run test -- src/app/build-performance.test.ts`
3. `npm run test`
4. `npm run build`
5. `node scripts/check-performance-budgets.mjs` (added in build phase)

## Testing Targets
- `src/app/accessibility-responsive-baselines.test.ts`
  - shell landmarks, skip link, responsive guardrails, reduced-motion baseline
- `src/app/shell.test.ts`
  - stable navigation order and keyboard target constants
- `src/app/router.test.ts`
  - route topology regression guard (`TASK-011` compatibility)
- `src/app/screens*.test.ts(x)` (add or extend in build phase)
  - state-role semantics and review form field-level validation associations
- `src/app/build-performance.test.ts`
  - bundling/chunking guardrails and budget-check wiring expectations
- `src/lib/events/use-daemon-events.test.ts` (if added/extended)
  - bounded event retention and display assumptions
- `src/lib/telemetry/store.test.ts`
  - diagnostics capacity behavior
- `scripts/check-performance-budgets.mjs`
  - validates referenced JS/CSS artifact budget compliance

## Regression Guardrails
- Do not alter route path declarations in `router.tsx`.
- Do not alter endpoint paths or envelope parse contracts in API client.
- Keep diagnostics panel filtering/correlation workflows intact.
- Preserve project context precedence behavior in `project-context.tsx`.

## Deferred Follow-Ups (Not in TASK-017)
- Full WCAG audit automation tooling integration (e.g., extended axe pipelines).
- Fine-grained render profiling dashboards across all routes.
- Virtualized rendering for very large JSON payload panels.
