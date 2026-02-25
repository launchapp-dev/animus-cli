# TASK-018 Wireframes: Web GUI CI, Smoke E2E, and Release Gates

Concrete wireframes for CI/release-gating UX in the standalone daemon web GUI.
These boards focus on deterministic gate visibility, failure evidence lookup,
release go/no-go decisions, and rollback confidence validation.

## Files
- `wireframes.html`: visual boards for checks, release gates, checklist, and rollback flow.
- `wireframes.css`: shared style system with responsive and accessibility constraints.
- `release-gates-wireframe.tsx`: React-oriented component/state scaffold for handoff.

## Screen Coverage

| Screen | Covered in |
| --- | --- |
| PR checks triage (`web-ui-ci`) | `wireframes.html` (`PR Checks + Matrix + Smoke Failure`) + `release-gates-wireframe.tsx` (`WebUiCiRunScreen`) |
| Release workflow gates (`release.yml`) | `wireframes.html` (`Release Workflow Gate Topology`) + `release-gates-wireframe.tsx` (`ReleaseGateTopologyScreen`) |
| Release checklist (`web-gui-release.md`) | `wireframes.html` (`Release Checklist`) + `release-gates-wireframe.tsx` (`ReleaseChecklistScreen`) |
| Rollback validation dispatch and summary | `wireframes.html` (`Rollback Validation`) + `release-gates-wireframe.tsx` (`RollbackValidationScreen`) |
| Mobile checks triage (`320px`) | `wireframes.html` (`Mobile Smoke Failure Triage`) + responsive notes in `wireframes.css` |

## State Coverage
- Gate/job state: `queued`, `running`, `passed`, `failed`, `blocked`, `cancelled`
- Checklist state: `draft`, `ready-for-go`, `blocked`, `signed-off`
- Rollback validation state: `idle`, `submitted`, `candidate-failed`, `rollback-failed`, `both-passed`
- Evidence state: `missing`, `linked`, `downloaded`

## Deterministic Evidence Modeled
- Trigger contract for `pull_request` and `push` with explicit path filter context.
- Matrix rows for Node `20.x` and `22.x` with explicit runtime and textual status.
- Smoke assertion labels tied to route/API checks (`/`, `/dashboard`, `/projects`, `/reviews/handoff`, `/api/v1/system/info`, `api_only=true` rejection).
- Stable artifact naming for smoke failures:
  - `web-ui-smoke-e2e-server-log`
  - `web-ui-smoke-e2e-assertions`
- Artifact evidence lifecycle examples: `missing`, `linked`, `downloaded`.
- Release gate dependency chain: `web-ui-gates -> build matrix -> publish`.
- Rollback summary with side-by-side outcomes for `candidate_ref` and `rollback_ref`.

## Accessibility and Responsive Intent
- Every status includes explicit text (`passed`, `failed`, `blocked`), not color only.
- Lifecycle/status legend includes terminal `cancelled` state in addition to pass/fail blockers.
- Forms use label-to-control associations and helper text for field intent.
- Live status regions are modeled with `aria-live="polite"` for run updates.
- Primary controls maintain `44px` minimum target height.
- Mobile board is constrained to `320px` and avoids horizontal page scrolling.

## Acceptance Criteria Traceability

| AC | Wireframe trace |
| --- | --- |
| `AC-01` | Web UI CI workflow board with path-trigger context and deterministic job names |
| `AC-02` | Matrix rows for Node `20.x` and `22.x` in checks table |
| `AC-03` | Smoke command and run state in checks board + rollback board |
| `AC-04` | Smoke assertions for UI routes and `/api/v1/system/info` envelope |
| `AC-05` | Explicit `api_only=true` deep-link rejection assertion row |
| `AC-06` | Release topology board with `web-ui-gates` job details |
| `AC-07` | Blocked build/publish lane when `web-ui-gates` fails |
| `AC-08` | Preserve packaging behavior note when gates pass |
| `AC-09` | Checklist board maps to `.github/release-checklists/web-gui-release.md` fields |
| `AC-10` | Rollback dispatch form includes `candidate_ref` and `rollback_ref` |
| `AC-11` | Rollback summary panel emits auditable per-ref outcomes |
| `AC-12` | Smoke failure board includes deterministic artifact upload evidence |
