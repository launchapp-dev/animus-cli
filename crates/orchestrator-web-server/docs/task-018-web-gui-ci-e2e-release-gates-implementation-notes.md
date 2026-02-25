# TASK-018 Implementation Notes: Web GUI CI Matrix, Smoke E2E, and Release Gates

## Purpose
Translate `TASK-018` requirements into deterministic implementation slices for
CI/release hardening without changing web API contracts or release artifact
formats.

## Non-Negotiable Constraints
- Keep existing release triggers and publish semantics in
  `.github/workflows/release.yml`.
- Preserve release artifact naming conventions already consumed by operators.
- Keep checks repository-local and reproducible from this repo.
- Use lockfile-faithful installs (`npm ci`) in CI.
- Do not manually edit `.ao` JSON state files.

## Baseline Integration Points
- Existing release workflow:
  `.github/workflows/release.yml`
- Web UI scripts and lockfile:
  `crates/orchestrator-web-server/web-ui/package.json`,
  `crates/orchestrator-web-server/web-ui/package-lock.json`
- Web UI build target:
  `crates/orchestrator-web-server/web-ui/vite.config.ts`
- Embedded asset output:
  `crates/orchestrator-web-server/embedded/`
- Existing web-server route behavior tests:
  `crates/orchestrator-web-server/src/services/web_server.rs`

## Proposed Source Layout Additions
- `.github/workflows/web-ui-ci.yml`
  - frontend matrix workflow for test/build and smoke E2E.
- `.github/workflows/release-rollback-validation.yml`
  - manual rollback confidence workflow using smoke checks.
- `.github/release-checklists/web-gui-release.md`
  - release-governance checklist with explicit gate evidence.
- `crates/orchestrator-web-server/web-ui/scripts/smoke-e2e.mjs`
  - deterministic smoke script that starts the AO web server and validates key
    routes/API behavior.
- `crates/orchestrator-web-server/web-ui/package.json`
  - add `test:e2e:smoke` script.

## CI Workflow Design Notes

### 1) `web-ui-ci.yml` topology
- Job A: `web-ui-matrix`
  - matrix: Node `20.x` and `22.x` (ubuntu).
  - run `npm ci`, `npm run test`, `npm run build`.
- Job B: `web-ui-smoke-e2e`
  - single-run job (non-matrix) to reduce runtime/cost.
  - runs after one successful matrix build job or after a dedicated setup step.
  - executes `npm run test:e2e:smoke`.
- Workflow permissions:
  - default `contents: read`.

### 2) Release gating in `release.yml`
- Add `web-ui-gates` job before existing `build` matrix.
- Move web GUI checks into `web-ui-gates`:
  - `npm ci`,
  - `npm run test`,
  - `npm run build`,
  - `npm run test:e2e:smoke`.
- Update dependency graph:
  - `build` needs `web-ui-gates`,
  - `publish` continues to need `build`.
- Preserve existing packaging scripts and artifact upload steps.

### 3) Rollback validation workflow
- `workflow_dispatch` with required refs:
  - `candidate_ref`,
  - `rollback_ref`.
- Per-ref smoke validation:
  - checkout ref,
  - run web GUI smoke script,
  - append pass/fail status to workflow summary.
- Never mutate tags/releases from rollback workflow.

## Smoke Harness Notes
- Script should be deterministic and self-cleaning:
  - spawn AO web server process,
  - wait for readiness with bounded timeout,
  - assert expected status/content-type/envelope fields,
  - trap process termination for pass/fail paths.
- Required assertions:
  - UI routes return `200` + `text/html`,
  - `/api/v1/system/info` returns `ao.cli.v1` success envelope,
  - `api_only=true` rejects UI deep links.
- Write smoke logs to a predictable temp file path so CI can upload artifacts on
  failure.

## Suggested Implementation Sequence
1. Add release checklist markdown artifact.
2. Add smoke E2E script and `package.json` command.
3. Add `web-ui-ci.yml` matrix + smoke jobs.
4. Integrate blocking `web-ui-gates` into `release.yml`.
5. Add `release-rollback-validation.yml`.
6. Run local smoke script and targeted workflow lint/validation.
7. Validate release workflow graph for fail-closed behavior.

## Testing Targets
- Local:
  - `cd crates/orchestrator-web-server/web-ui && npm ci`
  - `npm run test`
  - `npm run build`
  - `npm run test:e2e:smoke`
- CI:
  - verify matrix job fanout and pass/fail behavior,
  - verify release job blocking on web GUI gate failure,
  - verify rollback workflow summary for both refs.

## Regression Guardrails
- Do not change release artifact filenames/paths consumed by existing release
  automation.
- Do not alter `/api/v1` envelope semantics during smoke instrumentation.
- Avoid workflow drift by centralizing repeated commands in npm scripts.
- Keep CI path filters constrained so unrelated changes do not over-trigger runs.

## Deferred Follow-Ups (Not in TASK-018)
- Full browser-matrix E2E (multi-engine Playwright).
- Visual regression snapshots.
- Automated rollback execution beyond validation.
