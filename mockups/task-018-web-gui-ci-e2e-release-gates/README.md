# TASK-018 Wireframes: Web GUI CI, Smoke E2E, and Release Gates

Concrete wireframes for CI/release-gating UX in the standalone daemon web GUI.
These boards focus on deterministic gate visibility, failure evidence lookup,
release go/no-go decisions, rollback confidence validation, and command-center run triage.

## Files
- `wireframes.html`: visual boards for command center, checks, release gates, checklist, rollback flow, and ready-to-release sign-off.
- `wireframes.css`: shared style system with responsive and accessibility constraints.
- `release-gates-wireframe.tsx`: React-oriented component/state scaffold for handoff.

## Screen Coverage

| Screen | Covered in |
| --- | --- |
| Release gate command center (run triage + decision drawer) | `wireframes.html` (`Release Gate Command Center`) + `release-gates-wireframe.tsx` (`ReleaseGateCommandCenterScreen`) |
| PR checks triage (`web-ui-ci`) | `wireframes.html` (`PR Checks + Matrix + Smoke Failure`) + `release-gates-wireframe.tsx` (`WebUiCiRunScreen`) |
| Release workflow gates (`release.yml`) | `wireframes.html` (`Release Workflow Gate Topology`) + `release-gates-wireframe.tsx` (`ReleaseGateTopologyScreen`) |
| Release checklist (`web-gui-release.md`) | `wireframes.html` (`Release Checklist`) + `release-gates-wireframe.tsx` (`ReleaseChecklistScreen`) |
| Rollback validation dispatch and summary | `wireframes.html` (`Rollback Validation`) + `release-gates-wireframe.tsx` (`RollbackValidationScreen`) |
| Release go confirmation (`all gates passed`) | `wireframes.html` (`Release Decision - Go Ready`) + `release-gates-wireframe.tsx` (`ReleaseGoDecisionScreen`) |
| Mobile checks triage (`320px`) | `wireframes.html` (`Mobile Smoke Failure Triage`) + responsive notes in `wireframes.css` |

## State Coverage
- Gate/job state: `queued`, `running`, `passed`, `failed`, `blocked`, `cancelled`
- Checklist state: `draft`, `ready-for-go`, `blocked`, `signed-off`
- Rollback validation state: `idle`, `validation-error`, `submitted`, `candidate-failed`, `rollback-failed`, `both-passed`
- Evidence state: `missing`, `linked`, `downloaded`
- Decision drawer state: `missing-evidence`, `ready-for-go`, `blocked`

## Deterministic Evidence Modeled
- Trigger contract for `pull_request` and `push` with explicit path filter context.
- Trigger paths explicitly include:
  - `crates/orchestrator-web-server/**`
  - `.github/workflows/web-ui-ci.yml`
  - `.github/workflows/release.yml`
  - `.github/workflows/release-rollback-validation.yml`
  - `.github/release-checklists/web-gui-release.md`
- Matrix rows for Node `20.x` and `22.x` with explicit runtime and textual status.
- Required check labels are modeled exactly as branch-protection contract:
  - `web-ui-matrix (node 20.x)`
  - `web-ui-matrix (node 22.x)`
  - `web-ui-smoke-e2e`
  - `Web UI Gates`
- Smoke assertion labels tied to route/API checks (`/`, `/dashboard`, `/projects`, `/reviews/handoff`, `/api/v1/system/info`, `api_only=true` rejection).
- Stable artifact naming for smoke failures:
  - `web-ui-smoke-e2e-server-log`
  - `web-ui-smoke-e2e-assertions`
- Artifact evidence lifecycle examples: `missing`, `linked`, `downloaded`.
- Release gate dependency chain: `web-ui-gates -> build matrix -> publish`.
- Command-center run history with deterministic run IDs and blocker text.
- Checklist defaults remain fail-closed (`No-Go`) while smoke or rollback evidence is incomplete.
- Explicit Go-ready run (`#1103`) models success path after smoke fix and rollback validation completion.
- Rollback summary with side-by-side outcomes for `candidate_ref` and `rollback_ref`.
- Scope-lock guardrail note is included so implementation edits stay in the approved TASK-018 file set.

## Accessibility and Responsive Intent
- Every status includes explicit text (`passed`, `failed`, `blocked`), not color only.
- Lifecycle/status legend includes terminal `cancelled` state in addition to pass/fail blockers.
- Forms use label-to-control associations and helper text for field intent.
- Live status regions are modeled with `aria-live="polite"` for run updates.
- Primary controls maintain `44px` minimum target height.
- Command-center navigation rail collapses into a button strip for tablet widths.
- Release decision board includes keyboard-first sign-off flow with focused confirmation action.
- Mobile board is constrained to `320px` and avoids horizontal page scrolling.

## Acceptance Criteria Traceability

| AC | Wireframe trace |
| --- | --- |
| `AC-01` | Web UI CI workflow board with path-trigger context and deterministic job names |
| `AC-02` | Matrix rows for Node `20.x` and `22.x` in checks table |
| `AC-03` | Smoke command and run state in checks board + rollback board |
| `AC-04` | Smoke assertions for UI routes and `/api/v1/system/info` envelope |
| `AC-05` | Explicit `api_only=true` deep-link rejection assertion row |
| `AC-06` | Release topology + command-center board with `web-ui-gates` blocker details |
| `AC-07` | Blocked build/publish lane when `web-ui-gates` fails |
| `AC-08` | Preserve packaging behavior note when gates pass |
| `AC-09` | Checklist + command-center decision surfaces model both no-go blockers and go-ready sign-off evidence |
| `AC-10` | Rollback dispatch form includes `candidate_ref` and `rollback_ref` |
| `AC-11` | Rollback summary panel emits auditable per-ref outcomes |
| `AC-12` | Smoke failure board includes deterministic artifact upload evidence |
| `AC-13` | Required check names are explicitly modeled with exact labels for branch protection and release gates |
| `AC-14` | Scope-lock guardrail callout constrains implementation edits to the approved TASK-018 file set |
