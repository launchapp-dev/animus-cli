# TASK-018 Requirements: Web GUI CI Matrix, Smoke E2E, and Release Gates

## Phase
- Workflow phase: `requirements`
- Workflow ID: `d17af114-e8ad-496e-9b4e-c8582fb72797`
- Task: `TASK-018`

## Objective
Define production-ready CI and release gates for the standalone daemon web GUI
so changes are validated before artifact publication and rollback paths are
explicitly verifiable.

Primary outcomes:
- deterministic frontend test/build matrix in GitHub Actions,
- repository-local smoke E2E validation for web server + UI routing behavior,
- release checklist with explicit go/no-go criteria,
- rollback validation workflow with auditable outputs.

## Existing Baseline
- Current release automation is in `.github/workflows/release.yml`.
- Release workflow builds and publishes Rust binaries only; it does not run
  web-ui tests or smoke E2E validation.
- `crates/orchestrator-web-server/web-ui/package.json` has:
  - `npm run test` (Vitest),
  - `npm run build` (Vite to `../embedded`),
  but no smoke E2E command.
- There is no dedicated web GUI CI workflow under `.github/workflows/`.
- There is no checked-in release checklist or rollback-validation workflow.

## Scope
In scope for implementation after this requirements phase:
- Add a dedicated web GUI CI workflow with a deterministic frontend test matrix.
- Add smoke E2E validation that exercises the locally started AO web server and
  route behavior.
- Add release gates so release packaging/publish are blocked when web GUI gates
  fail.
- Add a release checklist artifact for operators.
- Add a rollback validation workflow for manual runbook verification.

Out of scope for this task:
- Visual-regression snapshots or full browser-matrix automation.
- Production deployment automation outside GitHub Actions.
- Signed artifact provenance/SBOM generation.
- API contract/schema changes for `/api/v1`.

## Constraints
- Preserve existing release triggers:
  - tag push `v*`,
  - branch push `version/**`.
- Preserve existing release artifact names and publish behavior from
  `release.yml` when gates pass.
- Keep validation deterministic and repository-local:
  - no external test services,
  - no dependence on non-repo environments.
- Use lockfile-faithful dependency installs (`npm ci`) in CI workflows.
- Keep smoke validation bounded and deterministic with explicit timeout and
  cleanup on failure.
- Keep `.ao` state changes out of manual JSON edits.

## Functional Requirements

### FR-01: Frontend CI Matrix Workflow
- Add `.github/workflows/web-ui-ci.yml`.
- Trigger conditions:
  - `pull_request` and `push`,
  - path filters include:
    - `crates/orchestrator-web-server/**`,
    - `.github/workflows/web-ui-ci.yml`,
    - `.github/workflows/release.yml`.
- Matrix dimensions (minimum required):
  - OS: `ubuntu-latest`,
  - Node: `20.x`, `22.x`.
- Required matrix job steps:
  - checkout repository,
  - setup Node with npm cache keyed by `web-ui/package-lock.json`,
  - run `npm ci` in `crates/orchestrator-web-server/web-ui`,
  - run `npm run test`,
  - run `npm run build`.
- Workflow permissions default to least privilege (`contents: read`).

### FR-02: Smoke E2E Harness and CI Job
- Add repository-local smoke harness at:
  - `crates/orchestrator-web-server/web-ui/scripts/smoke-e2e.mjs`.
- Add package script:
  - `npm run test:e2e:smoke`.
- Smoke harness must:
  - start AO web server against repo root with explicit host/port,
  - wait for readiness with deterministic timeout,
  - verify HTTP `200` and `text/html` for representative UI routes:
    - `/`,
    - `/dashboard`,
    - `/projects`,
    - `/reviews/handoff`,
  - verify `/api/v1/system/info` responds with `ao.cli.v1` success envelope,
  - verify `api_only=true` rejects UI deep links with deterministic error
    envelope behavior,
  - always terminate spawned server processes.
- CI must run smoke validation at least once per workflow execution.

### FR-03: Release Gate Enforcement
- Extend `.github/workflows/release.yml` with a blocking web GUI gate job.
- Gate job must run before release build matrix and before publish.
- Gate job minimum checks:
  - web-ui dependency install,
  - unit/component tests,
  - production build,
  - smoke E2E script.
- If gate job fails:
  - binary packaging jobs must not run,
  - GitHub release publish must not run.
- Existing binary packaging matrix and artifact naming remain unchanged on pass.

### FR-04: Release Checklist Artifact
- Add checklist markdown artifact:
  - `.github/release-checklists/web-gui-release.md`.
- Checklist must include explicit completion points for:
  - frontend CI matrix run URL and success state,
  - smoke E2E run URL and success state,
  - release-gate run URL and success state,
  - embedded asset regeneration evidence,
  - operator go/no-go sign-off.
- Checklist must include rollback preconditions and rollback trigger reference.

### FR-05: Rollback Validation Workflow
- Add `.github/workflows/release-rollback-validation.yml`.
- Trigger:
  - `workflow_dispatch` only.
- Required inputs:
  - `candidate_ref` (ref intended for release),
  - `rollback_ref` (known-good fallback ref/tag).
- Workflow behavior:
  - run smoke E2E validation against `candidate_ref`,
  - run smoke E2E validation against `rollback_ref`,
  - emit deterministic step-summary results for both refs,
  - do not publish, delete, retag, or mutate release artifacts.

### FR-06: Failure Diagnostics Artifacting
- On smoke workflow failure, upload troubleshooting artifacts:
  - captured server stdout/stderr logs,
  - smoke assertion report output.
- Use deterministic artifact naming and finite retention.

## Non-Functional Requirements

### NFR-01: Determinism
- CI and release gate behavior must be reproducible for the same commit.
- Workflow job names and gating topology must remain stable for required-check
  configuration.

### NFR-02: Runtime and Cost
- Frontend matrix + smoke gates should stay within practical CI runtime for PRs.
- Smoke E2E scope is intentionally minimal to avoid flaky long-running suites.

### NFR-03: Security and Permissions
- CI workflows use least-privilege permissions by default.
- Release-publish permissions remain scoped to publishing job only.

## Acceptance Criteria
- `AC-01`: `.github/workflows/web-ui-ci.yml` exists with required triggers and
  path filters.
- `AC-02`: CI matrix validates web-ui test + build for Node `20.x` and `22.x`.
- `AC-03`: Smoke E2E script exists and is runnable via `npm run test:e2e:smoke`.
- `AC-04`: Smoke E2E validates UI route HTML responses and `/api/v1/system/info`
  envelope.
- `AC-05`: Smoke E2E validates `api_only=true` deep-link rejection behavior.
- `AC-06`: Release workflow includes a blocking web GUI gate job.
- `AC-07`: Release packaging/publish do not execute when web GUI gate fails.
- `AC-08`: Existing release artifact naming and publish-on-tag behavior remain
  unchanged when gates pass.
- `AC-09`: Release checklist exists at
  `.github/release-checklists/web-gui-release.md`.
- `AC-10`: Rollback validation workflow exists and accepts `candidate_ref` +
  `rollback_ref`.
- `AC-11`: Rollback workflow produces auditable summary output without mutating
  tags/releases.
- `AC-12`: Smoke failures upload deterministic troubleshooting artifacts.

## Testable Acceptance Checklist
- `T-01`: Validate `web-ui-ci.yml` trigger and path-filter behavior with a
  workflow lint/check.
- `T-02`: Validate matrix jobs run on Node `20.x` and `22.x`.
- `T-03`: Run `npm run test:e2e:smoke` locally against spawned AO web server.
- `T-04`: CI smoke job asserts required UI routes return HTML 200.
- `T-05`: CI smoke job asserts `/api/v1/system/info` envelope fields (`schema`,
  `ok`) are valid.
- `T-06`: CI smoke job asserts `api_only=true` rejects `/dashboard`.
- `T-07`: Simulated failure in web GUI gate prevents release build/publish jobs.
- `T-08`: Rollback workflow run validates both refs and emits summary evidence.
- `T-09`: Failure path uploads expected logs/artifacts.

## Acceptance Verification Matrix
| Requirement | Verification method |
| --- | --- |
| Frontend matrix workflow | `web-ui-ci.yml` run results for Node matrix |
| Smoke E2E behavior | `npm run test:e2e:smoke` assertions in CI logs |
| Release gate blocking | `release.yml` job dependency graph and failure-path runs |
| Release checklist availability | checked-in checklist artifact and release run references |
| Rollback validation workflow | manual `workflow_dispatch` run on candidate + rollback refs |
| Failure diagnostics | artifact upload assertions on smoke failure |

## Implementation Notes (Next Phase Input)
- Primary files expected in build phase:
  - `.github/workflows/web-ui-ci.yml`
  - `.github/workflows/release.yml` (gate integration)
  - `.github/workflows/release-rollback-validation.yml`
  - `.github/release-checklists/web-gui-release.md`
  - `crates/orchestrator-web-server/web-ui/package.json`
  - `crates/orchestrator-web-server/web-ui/scripts/smoke-e2e.mjs`
- Keep gating logic explicit and fail-closed for release paths.
- Prefer repository scripts over long inline workflow shell blocks for
  maintainability and deterministic local reproduction.
