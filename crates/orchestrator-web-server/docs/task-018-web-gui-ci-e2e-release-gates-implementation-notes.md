# TASK-018 Implementation Notes: Web GUI CI Matrix, Smoke E2E, and Release Gates

## Phase Context
- Workflow phase: `requirements`
- Workflow ID: `1b83370a-3c2c-42f2-aea0-43c84bf0002d`
- Task: `TASK-018`

## Purpose
Translate `TASK-018` requirements into deterministic implementation and
validation slices for CI/release hardening, without changing web API contracts
or release artifact formats.

## Non-Negotiable Constraints
- Preserve release triggers and publish semantics in
  `.github/workflows/release.yml`.
- Preserve release artifact naming conventions consumed by operators.
- Keep checks repository-local and reproducible from this repository.
- Use lockfile-faithful installs (`npm ci`) in CI jobs.
- Keep rollback validation read-only (no tag/release mutation).
- Do not manually edit `.ao` JSON state files.

## Baseline Integration Points
- Frontend CI workflow:
  `.github/workflows/web-ui-ci.yml`
- Release workflow:
  `.github/workflows/release.yml`
- Rollback validation workflow:
  `.github/workflows/release-rollback-validation.yml`
- Release checklist:
  `.github/release-checklists/web-gui-release.md`
- Smoke script + package scripts:
  `crates/orchestrator-web-server/web-ui/scripts/smoke-e2e.mjs`,
  `crates/orchestrator-web-server/web-ui/package.json`
- Web UI lockfile:
  `crates/orchestrator-web-server/web-ui/package-lock.json`

## Scope-Locked Edit Surface
Allowed implementation file edits:
- `.github/workflows/web-ui-ci.yml`
- `.github/workflows/release.yml` (web UI gate section only)
- `.github/workflows/release-rollback-validation.yml`
- `.github/release-checklists/web-gui-release.md`
- `crates/orchestrator-web-server/web-ui/package.json`
- `crates/orchestrator-web-server/web-ui/scripts/smoke-e2e.mjs`

Protected surfaces (must not change in TASK-018 implementation):
- Release packaging matrix targets/archives in `release.yml`.
- Publish-on-tag behavior and release artifact naming.
- `/api/v1` envelope and API contracts beyond smoke validation assertions.

## Deterministic Workflow Topology

### 1) `web-ui-ci.yml`
- `web-ui-matrix`:
  - Node `20.x` and `22.x`, `ubuntu-latest`.
  - Runs `npm ci`, `npm run test`, `npm run build`.
- `web-ui-smoke-e2e`:
  - depends on `web-ui-matrix`,
  - runs smoke (`npm run test:e2e:smoke`) and uploads smoke diagnostics on
    failure.
- Workflow-level defaults:
  - `contents: read`,
  - deterministic concurrency group (`web-ui-ci-${{ github.ref }}`).
- Required check names remain stable:
  - `web-ui-matrix (node 20.x)`,
  - `web-ui-matrix (node 22.x)`,
  - `web-ui-smoke-e2e`.

### 2) `release.yml` gating behavior
- `web-ui-gates` runs first and performs:
  - `npm ci`, `npm run test`, `npm run build`, `npm run test:e2e:smoke`.
- `build` matrix is hard-blocked via `needs: web-ui-gates`.
- `publish` remains tag-gated and downstream of `build`.
- Existing packaging matrix targets, archive formats, and artifact naming remain
  unchanged.
- Required gate check remains `Web UI Gates`.

### 3) `release-rollback-validation.yml`
- `workflow_dispatch` inputs:
  - `candidate_ref`,
  - `rollback_ref`.
- Per-ref smoke jobs:
  - `candidate_smoke`,
  - `rollback_smoke`.
- `summary` job:
  - writes auditable step-summary evidence,
  - enforces both smoke outcomes are `success`,
  - never performs publish/tag mutation actions.

### 4) Release checklist governance
- Checklist captures run URLs and go/no-go evidence for:
  - `web-ui-ci.yml` matrix,
  - smoke E2E,
  - release `web-ui-gates`,
  - rollback-validation run,
  - embedded asset regeneration.

## Smoke Harness Behavior Contract
- Spawns AO web server via `cargo run -p orchestrator-cli -- web serve` with
  explicit host/port.
- Waits for readiness using `/api/v1/system/info` with bounded timeout.
- Validates UI routes (`/`, `/dashboard`, `/projects`, `/reviews/handoff`) for
  `200` + `text/html`.
- Validates `/api/v1/system/info` envelope fields (`schema=ao.cli.v1`,
  `ok=true`).
- Validates `--api-only` deep-link rejection (`404`, `not_found`,
  `exit_code=3`).
- Always terminates spawned processes and writes deterministic artifacts:
  - `smoke-assertions.txt`,
  - `<server>.stdout.log`,
  - `<server>.stderr.log`.

## Suggested Implementation/Verification Sequence
1. Confirm checklist file includes all required evidence fields.
2. Confirm smoke script + `test:e2e:smoke` script contract matches requirements.
3. Validate `web-ui-ci.yml` matrix and smoke topology.
4. Validate `release.yml` dependency graph remains fail-closed.
5. Validate rollback workflow inputs, summary evidence, and pass criteria.
6. Verify required check-name stability against branch protection contract.
7. Run local smoke test and targeted workflow lint/checks.

## Testing Targets
- Local web UI checks:
  - `cd crates/orchestrator-web-server/web-ui`
  - `npm ci`
  - `npm run test`
  - `npm run build`
  - `npm run test:e2e:smoke`
- Workflow structure checks:
  - verify `web-ui-gates -> build -> publish` dependencies,
  - verify required check names are unchanged (`web-ui-matrix (node 20.x)`,
    `web-ui-matrix (node 22.x)`, `web-ui-smoke-e2e`, `Web UI Gates`),
  - verify rollback summary enforcement,
  - verify smoke-failure artifact upload paths and retention.

## Regression Guardrails
- Do not alter release artifact filenames/paths.
- Do not alter `/api/v1` envelope semantics while extending smoke checks.
- Keep workflow check names stable for branch protection configuration.
- Keep repeated command logic in package scripts where possible.

## Deferred Follow-Ups (Not in TASK-018)
- Full multi-browser E2E suite.
- Visual-regression snapshot pipeline.
- Automated rollback execution beyond validation evidence.
