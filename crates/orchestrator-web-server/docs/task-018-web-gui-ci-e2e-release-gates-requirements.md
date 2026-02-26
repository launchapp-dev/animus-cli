# TASK-018 Requirements: Web GUI CI Matrix, Smoke E2E, and Release Gates

## Phase
- Workflow phase: `requirements`
- Workflow ID: `1b83370a-3c2c-42f2-aea0-43c84bf0002d`
- Task: `TASK-018`
- Project root: `/Users/samishukri/ao-cli`

## Objective
Define and lock deterministic CI and release-gate behavior for the standalone
web GUI so frontend regressions are caught before binary artifact publication,
and rollback confidence is auditable.

Primary outcomes:
- deterministic frontend test/build matrix in GitHub Actions,
- repository-local smoke E2E validation for web server + route behavior,
- release checklist with explicit go/no-go evidence,
- rollback validation workflow with auditable summaries.

## Existing Baseline (Phase Snapshot)
- `.github/workflows/web-ui-ci.yml` exists with:
  - `web-ui-matrix` (Node `20.x` + `22.x`) for `npm ci`, `npm run test`,
    `npm run build`,
  - `web-ui-smoke-e2e` for `npm run test:e2e:smoke` and smoke diagnostics upload.
- `.github/workflows/release.yml` includes `web-ui-gates` and blocks `build`
  via `needs: web-ui-gates`.
- `.github/workflows/release-rollback-validation.yml` exists with
  `workflow_dispatch` inputs `candidate_ref` and `rollback_ref`, and a required
  summary gate.
- Smoke harness exists at
  `crates/orchestrator-web-server/web-ui/scripts/smoke-e2e.mjs`.
- Release checklist exists at
  `.github/release-checklists/web-gui-release.md`.

## Scope
In scope for TASK-018 delivery and validation:
- Frontend CI matrix for deterministic test/build execution across supported
  Node versions.
- Smoke E2E validation against a locally spawned AO web server.
- Release gating so packaging/publish are fail-closed behind web GUI checks.
- Release checklist with explicit operator evidence and sign-off points.
- Rollback validation workflow that proves candidate/rollback refs without
  mutating release state.

Out of scope for this task:
- Full browser-matrix automation (Playwright/Cypress multi-browser suites).
- Visual regression snapshots.
- Production deployment automation outside GitHub Actions.
- Signed provenance/SBOM generation.
- API contract/schema changes for `/api/v1`.

## Implementation Scope Lock (Requirements Handoff)
Implementation phase may change only:
- `.github/workflows/web-ui-ci.yml`
- `.github/workflows/release.yml` (web UI gating scope only)
- `.github/workflows/release-rollback-validation.yml`
- `.github/release-checklists/web-gui-release.md`
- `crates/orchestrator-web-server/web-ui/package.json`
- `crates/orchestrator-web-server/web-ui/scripts/smoke-e2e.mjs`

Implementation phase must not change:
- Release packaging matrix targets and archive formats in `release.yml`.
- Publish-on-tag semantics and release artifact naming conventions.
- `/api/v1` response envelope contracts or server API behavior unrelated to smoke assertions.

## Constraints
- Preserve release triggers in `.github/workflows/release.yml`:
  - tag push `v*`,
  - branch push `version/**`.
- Preserve release artifact names and publish behavior when gates pass.
- Keep validation repository-local and deterministic:
  - no external test services,
  - explicit timeouts and cleanup for smoke runner processes.
- Use lockfile-faithful dependency installs (`npm ci`) in CI.
- Keep workflow permissions least-privilege by default (`contents: read`).
- Keep rollback workflow read-only with no publish/tag mutation behavior.
- Keep required check names stable for branch protection compatibility:
  - `web-ui-matrix (node 20.x)`,
  - `web-ui-matrix (node 22.x)`,
  - `web-ui-smoke-e2e`,
  - `Web UI Gates`.
- Do not manually edit `.ao` JSON state files.

## Deterministic Gate Topology
- `.github/workflows/web-ui-ci.yml`
  - `web-ui-matrix` (Node `20.x`, `22.x`)
  - `web-ui-smoke-e2e` (depends on matrix; runs smoke + failure artifacts)
- `.github/workflows/release.yml`
  - `web-ui-gates` -> `build` matrix -> `publish`
- `.github/workflows/release-rollback-validation.yml`
  - `candidate_smoke` + `rollback_smoke` -> `summary` (hard pass criteria)

## Functional Requirements

### FR-01: Frontend CI Workflow and Matrix
- Workflow file: `.github/workflows/web-ui-ci.yml`.
- Trigger conditions:
  - `pull_request` and `push`,
  - path filters include:
    - `crates/orchestrator-web-server/**`,
    - `.github/workflows/web-ui-ci.yml`,
    - `.github/workflows/release.yml`,
    - `.github/workflows/release-rollback-validation.yml`,
    - `.github/release-checklists/web-gui-release.md`.
- Matrix job requirements:
  - job name pattern: `web-ui-matrix (node <version>)`,
  - OS: `ubuntu-latest`,
  - Node: `20.x`, `22.x`,
  - steps: checkout, setup-node cache, `npm ci`, `npm run test`,
    `npm run build`.
- Workflow default permissions: `contents: read`.

### FR-02: Smoke E2E Harness and CI Execution
- Smoke harness path:
  `crates/orchestrator-web-server/web-ui/scripts/smoke-e2e.mjs`.
- Package script:
  - `npm run test:e2e:smoke`.
- Smoke harness must:
  - start AO web server with explicit host/port against repo root,
  - wait for readiness with deterministic timeout,
  - validate HTTP `200` + `text/html` for routes:
    - `/`, `/dashboard`, `/projects`, `/reviews/handoff`,
  - validate `/api/v1/system/info` returns `ao.cli.v1` success envelope,
  - validate `api_only=true` rejects UI deep links with deterministic error
    envelope (`not_found`, exit code `3`),
  - terminate spawned server processes on pass/fail,
  - write deterministic artifacts (`smoke-assertions.txt`, stdout/stderr logs)
    under `SMOKE_ARTIFACT_DIR`.
- CI must execute smoke checks on each `web-ui-ci.yml` run and release-gate run.

### FR-03: Release Gate Enforcement
- `release.yml` must include blocking job `web-ui-gates` before binary builds.
- `build` job must declare `needs: web-ui-gates`.
- `publish` job must remain downstream of `build` and tag-gated.
- `web-ui-gates` checks must include:
  - `npm ci`, `npm run test`, `npm run build`, `npm run test:e2e:smoke`.
- On gate failure:
  - build matrix must not run,
  - publish must not run.
- Existing binary packaging matrix and artifact naming remain unchanged on pass.

### FR-04: Release Checklist Artifact
- Checklist file: `.github/release-checklists/web-gui-release.md`.
- Checklist must include explicit evidence capture points for:
  - web-ui matrix success,
  - smoke E2E success,
  - release `web-ui-gates` success,
  - embedded asset regeneration evidence,
  - operator go/no-go sign-off,
  - rollback readiness and trigger reference.

### FR-05: Rollback Validation Workflow
- Workflow file: `.github/workflows/release-rollback-validation.yml`.
- Trigger:
  - `workflow_dispatch` only.
- Required inputs:
  - `candidate_ref`,
  - `rollback_ref`.
- Workflow behavior:
  - run smoke validation against `candidate_ref`,
  - run smoke validation against `rollback_ref`,
  - emit deterministic `$GITHUB_STEP_SUMMARY` evidence,
  - fail when either smoke execution fails,
  - avoid publish/delete/tag mutation operations.

### FR-06: Failure Diagnostics Artifacting
- On smoke failure, workflow must upload deterministic troubleshooting artifacts
  using finite retention (`retention-days: 7`).
- Required artifact categories:
  - server stdout/stderr logs,
  - smoke assertion report output.

### FR-07: Required Check Name Stability
- Job/check names for required CI and release gates must remain stable:
  - `web-ui-matrix (node 20.x)`,
  - `web-ui-matrix (node 22.x)`,
  - `web-ui-smoke-e2e`,
  - `Web UI Gates`.
- If new supplementary jobs are added, they must not replace or rename the
  required gate job/check names above.

## Non-Functional Requirements

### NFR-01: Determinism
- CI/release behavior must be reproducible for identical commits.
- Gate topology and required-check names must remain stable.

### NFR-02: Runtime and Cost
- Matrix + smoke coverage must stay practical for PR feedback loops.
- Smoke scope remains intentionally small to avoid flaky long suites.

### NFR-03: Security and Permissions
- Least-privilege permissions for CI workflows by default.
- Elevated permissions (`contents: write`) only on publish job.

## Acceptance Criteria
- `AC-01`: `.github/workflows/web-ui-ci.yml` exists with required triggers,
  path filters, and `contents: read` default permissions.
- `AC-02`: `web-ui-matrix` validates `npm ci`, `npm run test`, and
  `npm run build` for Node `20.x` and `22.x`.
- `AC-03`: `web-ui-smoke-e2e` executes `npm run test:e2e:smoke` and uploads
  smoke diagnostics on failure.
- `AC-04`: Smoke harness validates HTML route responses and
  `/api/v1/system/info` envelope (`schema=ao.cli.v1`, `ok=true`).
- `AC-05`: Smoke harness validates `api_only=true` deep-link rejection with
  deterministic error envelope (`not_found`, `exit_code=3`).
- `AC-06`: `release.yml` contains blocking `web-ui-gates` job with required
  web UI checks.
- `AC-07`: `build` is blocked by `needs: web-ui-gates`, and `publish` is
  blocked behind `build`.
- `AC-08`: Existing release artifact naming and publish-on-tag behavior remain
  unchanged when gates pass.
- `AC-09`: Release checklist exists at
  `.github/release-checklists/web-gui-release.md` with required evidence fields.
- `AC-10`: Rollback workflow accepts `candidate_ref` and `rollback_ref` via
  `workflow_dispatch` inputs.
- `AC-11`: Rollback workflow produces deterministic summary evidence and fails
  when either ref smoke validation fails.
- `AC-12`: Smoke failure diagnostics upload uses deterministic naming and finite
  retention.
- `AC-13`: Required check names remain stable for branch protection:
  - `web-ui-matrix (node 20.x)`,
  - `web-ui-matrix (node 22.x)`,
  - `web-ui-smoke-e2e`,
  - `Web UI Gates`.
- `AC-14`: Implementation stays within the scope-locked file set and does not
  alter release packaging matrix or publish semantics.

## Testable Acceptance Checklist
- `T-01`: Validate `web-ui-ci.yml` trigger/path-filter behavior with workflow
  lint or dry-run checks.
- `T-02`: Confirm two matrix runs are present for Node `20.x` and `22.x`.
- `T-03`: Run `npm run test:e2e:smoke` locally from
  `crates/orchestrator-web-server/web-ui`.
- `T-04`: Verify smoke assertions for HTML route responses.
- `T-05`: Verify smoke assertion for `/api/v1/system/info` envelope fields
  (`schema`, `ok`).
- `T-06`: Verify smoke assertion for `api_only=true` deep-link rejection.
- `T-07`: Verify release dependency graph (`web-ui-gates` -> `build` ->
  `publish`) in workflow definitions.
- `T-08`: Run rollback workflow with candidate/rollback refs and confirm summary
  evidence for both.
- `T-09`: Force smoke failure and verify diagnostics artifact upload behavior.
- `T-10`: Validate required check names in workflow definitions match branch
  protection configuration contract.
- `T-11`: Diff-audit implementation changes to confirm release packaging/publish
  semantics were not modified outside allowed gate scope.

## Acceptance Verification Matrix
| Requirement | Verification method |
| --- | --- |
| Frontend matrix workflow | `web-ui-ci.yml` run results for Node matrix |
| Smoke E2E behavior | `npm run test:e2e:smoke` assertions + CI smoke logs |
| Release gate blocking | `release.yml` job dependency graph and failure-path runs |
| Release checklist availability | Checked-in checklist + run URLs captured during release |
| Rollback validation workflow | Manual `workflow_dispatch` run for candidate + rollback refs |
| Failure diagnostics | Uploaded smoke artifact payload on forced failure |
| Required check name stability | Workflow YAML job names match branch-protection check contract |

## Implementation Notes (Next Phase Input)
Primary files for implementation/verification:
- `.github/workflows/web-ui-ci.yml`
- `.github/workflows/release.yml`
- `.github/workflows/release-rollback-validation.yml`
- `.github/release-checklists/web-gui-release.md`
- `crates/orchestrator-web-server/web-ui/package.json`
- `crates/orchestrator-web-server/web-ui/scripts/smoke-e2e.mjs`

Execution guidance:
- Keep release gate logic explicit and fail-closed.
- Keep smoke runner behavior deterministic and self-cleaning.
- Prefer repository scripts over long inline workflow shell blocks for
  maintainability and local reproducibility.
