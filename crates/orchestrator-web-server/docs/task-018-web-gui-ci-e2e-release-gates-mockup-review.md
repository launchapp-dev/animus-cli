# TASK-018 Mockup Review: Web GUI CI, Smoke E2E, and Release Gates

## Phase
- Workflow phase: `mockup-review`
- Workflow ID: `1b83370a-3c2c-42f2-aea0-43c84bf0002d`
- Task: `TASK-018`

## Scope of Review
Reviewed `TASK-018` mockup artifacts against:
- `task-018-web-gui-ci-e2e-release-gates-requirements.md`
- `task-018-web-gui-ci-e2e-release-gates-ux-brief.md`
- linked requirement `REQ-018` acceptance intent (test discipline, release checklist, rollback confidence)

Reviewed artifacts:
- `mockups/task-018-web-gui-ci-e2e-release-gates/wireframes.html`
- `mockups/task-018-web-gui-ci-e2e-release-gates/wireframes.css`
- `mockups/task-018-web-gui-ci-e2e-release-gates/release-gates-wireframe.tsx`
- `mockups/task-018-web-gui-ci-e2e-release-gates/README.md`

## Mismatch Resolution Log

| Mismatch | Requirement/UX reference | Resolution |
| --- | --- | --- |
| Release checklist sections did not follow the mandated operator order and merged rollback/post-release concerns into generic checklist items | UX brief checklist structure + FR-04 checklist completeness | Split checklist wireframe into explicit sections: Metadata, Preflight, CI Gate Evidence, Decision, Rollback Readiness, Post-release Verification in both HTML and TSX wireframes |
| CI trigger contract was only partially visible (missing rollback workflow and release-checklist path filters) | FR-01 trigger requirements + AC-01 | Expanded checks-board and TSX trigger model to include the complete deterministic path set plus explicit `pull_request` and `push` events |
| Smoke assertion coverage was implied by a single failure snippet instead of explicitly showing route/API contract scope | FR-02 + AC-04/AC-05 | Added explicit smoke assertion contract list covering UI routes, `/api/v1/system/info` envelope, and `api_only=true` rejection behavior |
| Artifact diagnostics modeled names but not lifecycle state clarity (`missing/linked/downloaded`) | FR-06 + UX evidence-first principle | Added artifact evidence-state section in wireframes and explicit evidence-state field in TSX artifact model |
| Gate status taxonomy omitted visible `cancelled` presentation in the board | NFR-01 deterministic gate semantics + README state coverage | Added `cancelled` status chip to HTML state strip and gate lifecycle list in TSX wireframe |
| Required check-name contract was not explicit and matrix row labels used generic names instead of branch-protection check names | FR-07 + AC-13 | Updated HTML and TSX wireframes to show exact required labels: `web-ui-matrix (node 20.x)`, `web-ui-matrix (node 22.x)`, `web-ui-smoke-e2e`, and `Web UI Gates` |
| Checklist decision model defaulted to `Go` while smoke/release gate evidence was failing and lacked explicit operator sign-off affordance | FR-04 checklist sign-off + fail-closed release UX intent | Updated checklist wireframe/TSX defaults to fail-closed `No-Go` with blocker notes and added explicit operator go/no-go sign-off control text |
| Rollback summary lacked explicit, copyable non-mutation audit output | FR-05 + AC-11 | Added deterministic step-summary excerpt (`candidate_ref`, `rollback_ref`, `mutation=none`, `publish=disabled`) to HTML and TSX rollback boards |
| Rollback dispatch model omitted the UX-required `validation-error` intermediate state | UX brief rollback dispatch state model | Added dispatch-state representation (`idle`, `validation-error`, `submitted`) in HTML and TSX rollback wireframes |
| Wide tables had higher narrow-screen overflow risk in mockup rendering | UX responsive readability guidance | Added table overflow wrapper styling (`table-wrap`) to preserve readability without page-level horizontal scroll |
| AC traceability did not include scope-lock acceptance coverage | AC-14 + implementation scope lock | Added explicit scope-lock guardrail callout to wireframes and expanded README/TSX acceptance mapping with AC-14 evidence references |

## Acceptance Criteria Traceability (Mockup Phase)

| AC | Evidence |
| --- | --- |
| `AC-01` | Checks board and TSX model show trigger events plus complete path filters (`crates/orchestrator-web-server/**`, workflow files, release checklist) and stable job naming (`wireframes.html`, `release-gates-wireframe.tsx`) |
| `AC-02` | Node `20.x` and `22.x` matrix rows remain explicit in checks table and TSX data model (`wireframes.html`, `release-gates-wireframe.tsx`) |
| `AC-03` | Smoke job remains represented as required gate in CI board and release topology (`wireframes.html`, `release-gates-wireframe.tsx`) |
| `AC-04` | Explicit smoke assertion contract includes UI route HTML and `/api/v1/system/info` envelope checks (`wireframes.html`, `release-gates-wireframe.tsx`) |
| `AC-05` | Explicit `api_only=true` deep-link rejection assertion retained in contract and diagnostics (`wireframes.html`, `release-gates-wireframe.tsx`) |
| `AC-06` | Release topology keeps blocking `web-ui-gates` prerequisite before build/publish (`wireframes.html`, `release-gates-wireframe.tsx`) |
| `AC-07` | Blocked build/publish state is explicit when web gate fails (`wireframes.html`, `release-gates-wireframe.tsx`) |
| `AC-08` | Passing-path note preserves existing artifact naming/publish behavior (`wireframes.html`, `release-gates-wireframe.tsx`) |
| `AC-09` | Checklist structure, fail-closed `No-Go` default when blockers remain, and explicit sign-off affordance map directly to release checklist artifact intent (`wireframes.html`, `release-gates-wireframe.tsx`, `README.md`) |
| `AC-10` | Rollback dispatch form keeps required `candidate_ref` and `rollback_ref` with clear helper copy (`wireframes.html`, `release-gates-wireframe.tsx`) |
| `AC-11` | Rollback summary now includes auditable per-ref outputs and explicit non-mutation line (`wireframes.html`, `release-gates-wireframe.tsx`) |
| `AC-12` | Deterministic failure artifact names plus evidence-state modeling and retention remain explicit (`wireframes.html`, `release-gates-wireframe.tsx`, `README.md`) |
| `AC-13` | Required check labels are now explicit and exact in checks/release boards and TSX data model (`wireframes.html`, `release-gates-wireframe.tsx`, `README.md`) |
| `AC-14` | Scope-lock guardrail callout and acceptance mapping constrain implementation edits to approved surfaces (`wireframes.html`, `release-gates-wireframe.tsx`, `README.md`) |

## Outcome
Mockups now provide complete AC-01 through AC-14 traceability, fail-closed checklist decision semantics, exact required-check naming coverage, tighter operator checklist hierarchy, and clearer accessibility/responsive behavior for CI triage, release go/no-go, and rollback confidence workflows.
