# Web GUI Release Checklist

Use this checklist for tag releases (`v*`) and release-preview branches
(`version/**`) that include web GUI changes.

## Release Metadata
- Release target ref/tag:
- Release workflow run URL:
- Operator:
- Date (UTC):

## Preflight
- [ ] `web-ui` lockfile is committed and `npm ci` succeeds.
- [ ] `npm run test` passes in `crates/orchestrator-web-server/web-ui`.
- [ ] `npm run build` succeeds and expected embedded assets are generated.
  - Embedded asset regeneration evidence URL:
- [ ] No unintended edits outside release scope.

## CI Gate Evidence
- [ ] `web-ui-ci.yml` matrix completed successfully for Node `20.x` and `22.x`.
  - Evidence URL:
- [ ] Smoke E2E check completed successfully.
  - Evidence URL:
- [ ] Release workflow `web-ui-gates` job completed successfully.
  - Evidence URL:

## Release Gate Decision
- [ ] Go decision recorded: all required checks green.
- [ ] Block decision recorded if any required check failed.
- [ ] Operator go/no-go sign-off recorded.
- Decision notes:

## Rollback Readiness
- [ ] `release-rollback-validation.yml` run executed for:
  - `candidate_ref`:
  - `rollback_ref`:
- [ ] Candidate and rollback smoke results captured in workflow summary.
- [ ] Rollback trigger reference documented (incident/release criteria).
- Rollback validation run URL:

## Post-Release Verification
- [ ] Published release assets include expected binaries/checksums.
- [ ] Sanity-check web GUI route loading against released artifact context.
- [ ] Any follow-up issues/tasks logged with explicit owner.
