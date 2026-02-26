# TASK-014 Implementation Notes: Task/Workflow Control Center Interface

## Purpose
Translate TASK-014 requirements into deterministic implementation slices across
web UI client modules for queue management, state transitions, workflow
controls, timeline rendering, and gating UX.

## Non-Negotiable Constraints
- Keep API calls under `/api/v1`.
- Preserve `ao.cli.v1` envelope parsing and normalized error model.
- Keep `.ao` mutations service-driven through existing API handlers only.
- Keep implementation scoped to task/workflow control surfaces.
- Keep high-impact actions fail-closed when gating data is incomplete.
- Keep keyboard and mobile usability baseline (`320px`) intact.

## Baseline Integration Points
- Route registry and shell navigation:
  - `crates/orchestrator-web-server/web-ui/src/app/router.tsx`
  - `crates/orchestrator-web-server/web-ui/src/app/shell.tsx`
- Current task/workflow route screens:
  - `crates/orchestrator-web-server/web-ui/src/app/screens.tsx`
- API transport and envelope normalization:
  - `crates/orchestrator-web-server/web-ui/src/lib/api/client.ts`
  - `crates/orchestrator-web-server/web-ui/src/lib/api/envelope.ts`
- API contracts and payload guards:
  - `crates/orchestrator-web-server/web-ui/src/lib/api/contracts/models.ts`
  - `crates/orchestrator-web-server/web-ui/src/lib/api/contracts/guards.ts`
- Diagnostics and telemetry:
  - `crates/orchestrator-web-server/web-ui/src/app/diagnostics-panel.tsx`
  - `crates/orchestrator-web-server/web-ui/src/lib/telemetry/*`

## Proposed Change Surface

### 1) Control-center UI modules
- Add task/workflow-focused UI modules and keep `screens.tsx` as route assembly:
  - `web-ui/src/app/task-control-center.tsx`
  - `web-ui/src/app/workflow-control-center.tsx`
  - `web-ui/src/app/workflow-phase-timeline.tsx`
  - `web-ui/src/app/action-gate-dialog.tsx`
  - `web-ui/src/app/action-feedback-log.tsx`
- Keep list/detail/control/timeline concerns separated to reduce route-level
  component complexity.

### 2) API client coverage completion
- Extend `src/lib/api/client.ts` with missing task/workflow controls:
  - task queue helpers: `tasksPrioritized`, `tasksNext`
  - task transitions/mutations: `taskUpdate`, `taskSetStatus`,
    `taskChecklistAdd`, `taskChecklistUpdate`, `taskDependencyAdd`,
    `taskDependencyRemove`
  - workflow controls: `workflowRun`, `workflowPause`, `workflowResume`,
    `workflowCancel`
- Ensure all mutating operations include explicit `actionName` for diagnostics.

### 3) Contract guard hardening
- Expand payload guards so UI can safely render queue cards and timeline rows
  without raw JSON fallback.
- Normalize status aliases (`todo`/`backlog`, `in_progress`/`in-progress`,
  `on_hold`/`on-hold`) before rendering control options.

### 4) High-impact gating architecture
- Implement a small action-risk registry for task/workflow actions:
  - low: safe immediate actions,
  - medium: explicit confirm click,
  - high: typed-phrase confirmation.
- Use one reusable gating dialog component with deterministic state machine:
  - `idle -> confirming -> submitting -> done|error -> idle`
- Fail-closed behavior:
  - block dispatch if target ID/action metadata/phrase is missing.

### 5) Timeline composition
- Build timeline rows from checkpoints + decisions with stable key derivation.
- Sorting rule:
  - checkpoint index ascending,
  - then timestamp ascending,
  - then key ascending.
- Render compact summary row + expandable details to preserve scanability.

### 6) Styling and hierarchy
- Add dedicated styles in `web-ui/src/styles.css` for:
  - queue summary strip,
  - queue/detail split layout,
  - control button groups with clear risk affordance,
  - timeline rows with status markers,
  - modal confirmation dialog and focus states.
- Keep desktop and mobile layouts explicit, with stacked fallback below `960px`.

## Recommended Build Sequence
1. Expand API client methods and tests for task/workflow control actions.
2. Add/strengthen payload models and decoders for control-center render fields.
3. Implement queue + workflow control components and wire into routes.
4. Implement phase timeline renderer and integrate into workflow detail.
5. Implement gating dialog + risk registry + action feedback log.
6. Integrate diagnostics panels for task/workflow action prefixes.
7. Finalize responsive/accessibility styles and interaction polish.
8. Run tests/build and resolve regressions.

## Testing Targets
- API client + decoder tests:
  - `web-ui/src/lib/api/client.test.ts`
  - `web-ui/src/lib/api/contracts/guards.test.ts`
- Route/component tests:
  - `web-ui/src/app/screens.test.tsx` (or split tests per new component)
  - task queue + transition + workflow control + timeline + gating coverage
- Regression checks:
  - envelope parsing tests
  - diagnostics integration expectations for `task.*` and `workflow.*`

## Validation Commands
- `npm --prefix crates/orchestrator-web-server/web-ui run test`
- `npm --prefix crates/orchestrator-web-server/web-ui run build`
- `cargo test -p orchestrator-web-server`

## Risks and Mitigations
- Risk: schema variance in task/workflow payloads causes brittle UI rendering.
  - Mitigation: broaden guard normalization and keep graceful fallback fields.
- Risk: action duplication/race conditions in rapid operator interaction.
  - Mitigation: centralized pending-action lock and per-action disable rules.
- Risk: gate UX regression from duplicated confirmation logic.
  - Mitigation: single reusable gate component and shared registry.
- Risk: timeline noise reduces operator signal.
  - Mitigation: concise summary rows with progressive detail disclosure.
