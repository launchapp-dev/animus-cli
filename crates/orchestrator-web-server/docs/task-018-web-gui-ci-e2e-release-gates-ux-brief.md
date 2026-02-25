# TASK-018 UX Brief: Web GUI CI, Smoke E2E, and Release Gates

## Phase
- Workflow phase: `ux-research`
- Workflow ID: `d17af114-e8ad-496e-9b4e-c8582fb72797`
- Task: `TASK-018`

## UX Objective
Design a deterministic release-gating experience that lets operators answer three
questions quickly:
1. Did web GUI checks pass?
2. If not, where is the failure evidence?
3. Is rollback confidence validated before publish decisions?

The workflow experience must keep pass/fail status explicit, preserve audit
evidence, and minimize ambiguity across pull request, release, and rollback
validation paths.

## Primary Users and Jobs

| User | Primary jobs | UX success signal |
| --- | --- | --- |
| PR author | Confirm web GUI checks pass before merge | Can identify matrix + smoke status from checks list in <= 2 clicks |
| Release operator | Make go/no-go decision on release runs | Can verify all required gate evidence from one checklist and one release run |
| On-call responder | Validate rollback candidate quickly during incidents | Can run rollback validation and compare candidate vs rollback outcomes in one summary view |

## UX Principles for This Phase
1. Gate visibility first: required checks must be easy to find and interpret.
2. Deterministic naming: jobs, steps, and artifacts use stable names for fast lookup.
3. Evidence over intuition: each decision point links to concrete run output.
4. Progressive detail: default view shows pass/fail and next action, with deep logs available on demand.
5. Accessible status language: do not rely on color alone; include explicit textual outcome.

## Information Architecture

### Primary Operator Entry Points
1. Pull request Checks tab.
2. `web-ui-ci.yml` workflow run summary.
3. `release.yml` run summary (`web-ui-gates` prerequisite).
4. `release-rollback-validation.yml` manual dispatch form and run summary.
5. `.github/release-checklists/web-gui-release.md` checklist artifact.

### Required Evidence Artifacts
1. Web UI matrix results (Node `20.x`, `22.x`).
2. Smoke E2E pass/fail output and assertion report.
3. Smoke failure logs (stdout/stderr + assertion report).
4. Release gate completion status before build/publish jobs.
5. Rollback validation summary for `candidate_ref` and `rollback_ref`.

## Key Screens and Interaction Contracts

| Screen | Goal | Primary interactions | Required states |
| --- | --- | --- | --- |
| PR Checks list | Decide merge readiness for web GUI changes | Open required checks, inspect failed check, jump to run details | pending, success, failure, cancelled |
| `web-ui-ci.yml` run summary | Verify matrix and smoke coverage | Expand matrix jobs, open smoke step logs, download failure artifacts | queued, running, passed, failed |
| `release.yml` run summary | Confirm release is blocked unless web gates pass | Inspect `web-ui-gates` status, confirm build jobs gated by dependency graph | blocked-by-gates, running, passed, failed |
| Web GUI release checklist (`.md`) | Record auditable go/no-go decision | Fill checklist fields, attach evidence URLs, record decision notes | draft, ready-for-go, blocked, signed-off |
| Rollback validation dispatch form | Launch deterministic candidate vs rollback smoke validation | Enter `candidate_ref`, enter `rollback_ref`, trigger run | idle, validation-error, submitted |
| Rollback validation run summary | Compare candidate and fallback confidence | Read per-ref outcome, inspect logs/artifacts, copy summary into incident/release notes | running, candidate-failed, rollback-failed, both-passed |

## Critical User Flows

### Flow A: Pull Request Gate Triage
1. PR author opens Checks tab after pushing web GUI changes.
2. Author confirms `web-ui-ci` required checks have completed.
3. On failure, author opens failing job and checks smoke assertion output and uploaded logs.
4. Author applies fix and re-runs until required checks are green.

### Flow B: Release Go/No-Go Decision
1. Release operator opens `release.yml` run for `v*` tag or `version/**` branch.
2. Operator verifies `web-ui-gates` succeeded before any publish path proceeds.
3. Operator updates release checklist with CI run URLs and explicit decision notes.
4. Publish proceeds only when checklist evidence and required jobs are both green.

### Flow C: Smoke Failure Diagnosis
1. Smoke step fails in CI or release gates.
2. Operator downloads deterministic failure artifacts.
3. Operator reviews route/API assertion output and server stdout/stderr logs.
4. Operator records blocker status and links evidence in checklist or incident notes.

### Flow D: Rollback Validation Confidence
1. Operator triggers `release-rollback-validation.yml` manually.
2. Inputs `candidate_ref` and `rollback_ref`.
3. Workflow runs smoke checks for both refs and emits side-by-side summary.
4. Operator uses summary to confirm rollback readiness without mutating tags/releases.

## Layout, Hierarchy, and Spacing Guidance

### GitHub Run Summary Content Order
1. Overall gate outcome.
2. Matrix/smoke results.
3. Failure evidence links.
4. Next-action guidance (retry/fix/escalate).

### Checklist Authoring Structure
- Keep sections in this order: Metadata -> Preflight -> CI Gate Evidence ->
  Decision -> Rollback Readiness -> Post-release Verification.
- Use short labels and one evidence URL slot per required gate item.
- Keep line lengths moderate so checklist remains readable in narrow viewports.

### Responsive Readability
- Avoid wide multi-column tables in step summaries; prefer bullet lists and short key-value lines.
- Keep artifact names concise and predictable to reduce truncation on mobile GitHub views.
- Ensure important statuses appear near the top of each summary to avoid long-scroll hunting.

## Accessibility Constraints (Non-Negotiable)
1. All workflow and checklist statuses must include explicit text (`passed`, `failed`, `blocked`) and not color-only meaning.
2. Checklist headings must follow logical order with no skipped heading level.
3. Every checklist checkbox label must be descriptive enough when read out of context by a screen reader.
4. Evidence links must use meaningful text or nearby labels that explain what the link proves.
5. Workflow summary output must remain understandable in plain text without requiring screenshots.
6. Input descriptions for `workflow_dispatch` fields must clearly distinguish `candidate_ref` vs `rollback_ref`.
7. Failure guidance must identify the next action in words, not just symbols or iconography.
8. Any copied command/reference text should be monospaced and line-break safe.

## Interaction Details

| Interaction | Expected behavior | Error prevention/recovery |
| --- | --- | --- |
| Open failed smoke step | User quickly sees failed assertion and route/API context | Always include deterministic assertion labels and artifact pointers |
| Download smoke artifacts | User retrieves logs from a stable artifact name | Upload logs on failure path with finite retention and documented name |
| Complete release checklist | User records evidence URLs and decision notes | Checklist requires explicit go/no-go acknowledgment |
| Run rollback validation | User compares candidate vs rollback results | Keep per-ref outcomes separated and clearly titled in summary |

## Risks and Mitigations

| Risk | Impact | Mitigation |
| --- | --- | --- |
| Gate status is hard to locate | Incorrect merge/release decisions | Stable required-check names and ordered summary sections |
| Smoke failures lack context | Slow recovery and reruns | Upload deterministic logs plus assertion report on failure |
| Checklist becomes stale or incomplete | Lost release auditability | Mandatory evidence URL slots and explicit decision section |
| Rollback refs are entered incorrectly | False confidence in rollback readiness | Clear input labels/descriptions and per-ref summary headings |

## UX Acceptance Checklist for Implementation Phase
- PR checks clearly expose web UI matrix and smoke outcome as required gates.
- Release workflow communicates that `web-ui-gates` is blocking build/publish.
- Smoke failure artifacts are easy to locate and identify from run logs.
- Release checklist supports explicit, auditable go/no-go decisions with evidence URLs.
- Rollback validation workflow input form and summary make candidate vs rollback outcomes unambiguous.
- Summary and checklist content remain readable and actionable on narrow/mobile GitHub layouts.
