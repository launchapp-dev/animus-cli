# TASK-002 Wireframes: CLI Help and Error Message Polish

Concrete wireframes for deterministic CLI guidance in `TASK-002`.
These artifacts model help discovery, invalid-input recovery, confirmation gates,
dry-run previews, and JSON parity for automation consumers.

## Files
- `wireframes.html`: desktop and mobile wireframe boards for core CLI surfaces.
- `wireframes.css`: visual system, hierarchy, spacing, and responsive behavior.
- `cli-help-error-wireframe.tsx`: React-oriented state scaffold for implementation handoff.

## Surface Coverage

| Surface | Covered in |
| --- | --- |
| Root help (`ao --help`) | `wireframes.html` (`Root + Group Help Hierarchy`) + `cli-help-error-wireframe.tsx` (`ROOT_HELP_LINES`, `SURFACES` entry `root-help`) |
| Group help (`ao task --help`) | `wireframes.html` (`Root + Group Help Hierarchy`) + `cli-help-error-wireframe.tsx` (`GROUP_HELP_LINES`, `SURFACES` entry `group-help`) |
| Command help (`ao task update --help`) | `wireframes.html` (`Command Help + Argument Clarity`) + `cli-help-error-wireframe.tsx` (`COMMAND_HELP_LINES`, `SURFACES` entry `command-help`) |
| Invalid-value recovery | `wireframes.html` (`Invalid Value Recovery + JSON Parity`) + `cli-help-error-wireframe.tsx` (`formatInvalidValueError`, `SURFACES` entry `validation`) |
| Confirmation-required gate | `wireframes.html` (`Confirmation Required + Dry Run`) + `cli-help-error-wireframe.tsx` (`formatConfirmationRequired`, `SURFACES` entry `destructive`) |
| Dry-run preview shape | `wireframes.html` (`Confirmation Required + Dry Run`) + `cli-help-error-wireframe.tsx` (`DESTRUCTIVE_PREVIEW`, `SHARED_DRY_RUN_KEYS`) |
| JSON envelope parity | `wireframes.html` (`Invalid Value Recovery + JSON Parity`) + `cli-help-error-wireframe.tsx` (`CliErrorEnvelope`, `CliSuccessEnvelope`) |

## Mockup-Review Resolutions
- Added `task-control` to root help command-group hierarchy to match scoped requirements coverage.
- Tightened invalid-value remediation to command-level help (`ao task update --help`) instead of broader group help.
- Corrected destructive git confirmation guidance to `--confirmation-id <id>` and added explicit `--confirm <token>` workflow variant.
- Added requirement-status invalid-value example in the TSX validation surface for bounded-domain coverage beyond task status.
- Updated traceability language for `AC-10` to emphasize deterministic fixture strings usable in CLI regression tests.

## State Coverage
- Help flow: `discovery`, `selection`, `ready`
- Validation flow: `error-shown`, `corrected-rerun`
- Destructive flow: `confirmation-blocked`, `preview-rendered`, `confirmed-rerun`
- JSON flow: `error-json`, `success-json`

## Canonical Contracts Modeled
- Invalid-value message:
  `invalid <domain> '<value>'; expected one of: <v1>, <v2>, ...; run '<command> --help'`
- Confirmation-required message:
  `CONFIRMATION_REQUIRED: rerun '<command>' with <confirmation flag> <token>; use --dry-run to preview changes`
- Shared dry-run keys:
  `operation`, `target`, `destructive`, `dry_run`, `requires_confirmation`, `planned_effects`, `next_step`

## Accessibility and Responsive Intent
- Plain-text terminal-first rendering with no color-only meaning.
- Keyboard-visible focus states on all interactive controls.
- Message clauses kept deterministic and copy/paste safe (ASCII punctuation).
- Mobile board explicitly modeled at `320px`, with wrapped command lines and no horizontal scroll.
- Help and remediation copy keep explicit flag names (`--help`, `--dry-run`, `--confirm`, `--confirmation-id`).

## Acceptance Criteria Traceability

| AC | Trace |
| --- | --- |
| `AC-01` | Root/group help board intent sections and scoped about text (`wireframes.html`, `ROOT_HELP_LINES`, `GROUP_HELP_LINES`) |
| `AC-02` | Command help board argument guidance with formats/defaults (`wireframes.html`, `COMMAND_HELP_LINES`) |
| `AC-03` | `--input-json` precedence callouts in command help board and React scaffold (`wireframes.html`, `COMMAND_HELP_LINES`) |
| `AC-04` | Invalid-value board with deterministic accepted values + rerun hint (`wireframes.html`, `formatInvalidValueError`) |
| `AC-05` | Confirmation gate board with canonical `CONFIRMATION_REQUIRED` wording (`wireframes.html`, `formatConfirmationRequired`) |
| `AC-06` | Dry-run preview board includes shared key set in stable order (`wireframes.html`, `SHARED_DRY_RUN_KEYS`) |
| `AC-07` | JSON envelope examples preserve `ao.cli.v1` error/success semantics (`wireframes.html`, `CliErrorEnvelope`, `CliSuccessEnvelope`) |
| `AC-08` | Exit-code mapping shown in JSON error examples (`wireframes.html`) |
| `AC-09` | Destructive gate and dry-run-before-confirm sequence represented (`wireframes.html`, `DESTRUCTIVE_PREVIEW`) |
| `AC-10` | Deterministic help/error fixture strings are available for smoke/e2e assertion authoring (`ROOT_HELP_LINES`, `COMMAND_HELP_LINES`, formatter helpers) |
| `AC-11` | Canonical token order preserved in formatter helpers (`cli-help-error-wireframe.tsx`) |
| `AC-12` | No time/host-dependent phrases in static help/error templates (`wireframes.html`, `cli-help-error-wireframe.tsx`) |
