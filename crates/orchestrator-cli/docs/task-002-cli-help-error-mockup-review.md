# TASK-002 Mockup Review: CLI Help and Error Messages

## Phase
- Workflow phase: `mockup-review`
- Workflow ID: `4e289849-5501-4332-ba0d-e907038390ce`
- Task: `TASK-002`

## Review Goal
Validate task mockups against `task-002-cli-help-error-requirements.md`, resolve requirement mismatches, and preserve deterministic copy for downstream implementation and testing.

## Mismatch Audit and Resolutions

| Requirement Area | Mismatch Found | Resolution Applied |
| --- | --- | --- |
| Scoped command help (`AC-01`) | Root help mockup omitted `task-control` from core command groups. | Added `task-control` command group line in root help wireframe and TSX scaffold constants. |
| Actionable invalid-value guidance (`AC-04`) | Invalid status examples pointed to broad group help (`ao task --help`) instead of command-level remediation. | Updated invalid-value messages to `run 'ao task update --help'` in desktop/mobile wireframes and TSX formatter usage. |
| Confirmation flag clarity (`AC-05`) | Git destructive mockup incorrectly used `--confirm`; actual git destructive paths use `--confirmation-id`. | Updated git confirmation examples to `--confirmation-id CONF-7F3A` and retained explicit task/workflow `--confirm` variant in mockup output. |
| Bounded-domain coverage breadth (`FR-02`) | Validation examples showed task status only. | Added requirement-status invalid-value example in TSX validation surface (`ao requirements update ... --status waiting`). |
| Acceptance traceability (`AC-10`) | README traceability text for `AC-10` described copy style, not test-oriented fixture readiness from requirements. | Updated README mapping to explicitly tie deterministic strings/helpers to smoke/e2e assertion authoring. |

## Files Updated in This Review
- `mockups/task-002-cli-help-error/wireframes.html`
- `mockups/task-002-cli-help-error/cli-help-error-wireframe.tsx`
- `mockups/task-002-cli-help-error/README.md`

## Determinism and Accessibility Checks
- Message templates preserve stable clause order and ASCII-safe punctuation.
- Recovery hints retain explicit flag names (`--help`, `--dry-run`, `--confirm`, `--confirmation-id`).
- Mobile wireframe continues to wrap long remediation text without truncating critical tokens.

## Acceptance Traceability Status
- `AC-01` through `AC-12`: represented in updated wireframe/TSX/README artifacts with corrected flag semantics and deterministic wording contracts.
