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
| Scoped group audit (`task-control`, `workflow`, `requirements`, `git`) | `wireframes.html` (`Scoped Command Group Audit Matrix`) + `cli-help-error-wireframe.tsx` (`SURFACES` entry `group-audit`, `*_HELP_LINES`) |
| Command help (`ao task update --help`) | `wireframes.html` (`Command Help + Argument Clarity`) + `cli-help-error-wireframe.tsx` (`COMMAND_HELP_LINES`, `SURFACES` entry `command-help`) |
| Invalid-value recovery | `wireframes.html` (`Invalid Value Recovery + JSON Parity`) + `cli-help-error-wireframe.tsx` (`formatInvalidValueError`, `SURFACES` entry `validation`) |
| Confirmation-required gate | `wireframes.html` (`Confirmation Required + Dry Run`) + `cli-help-error-wireframe.tsx` (`formatConfirmationRequired`, `formatGitConfirmationRequired`, `SURFACES` entry `destructive`) |
| Dry-run preview shape | `wireframes.html` (`Confirmation Required + Dry Run`) + `cli-help-error-wireframe.tsx` (`DESTRUCTIVE_PREVIEW`, `SHARED_DRY_RUN_KEYS`) |
| JSON envelope parity | `wireframes.html` (`Invalid Value Recovery + JSON Parity`) + `cli-help-error-wireframe.tsx` (`CliErrorEnvelope`, `CliSuccessEnvelope`) |

## Wireframe Polish Updates
- Added a scoped command-group audit board covering `task-control`, `workflow`, `requirements`, and `git` help consistency.
- Aligned invalid-value examples to canonical parser wording: `run the same command with --help`.
- Aligned git confirmation guidance to canonical request/approve wording plus `--confirmation-id <id>`.
- Updated dry-run preview contract examples to include the shared `action` key.
- Kept task/workflow destructive confirmation guidance explicit with `--confirm <token>`.

## Mockup-Review Corrections
- Corrected CLI command naming drift in help surfaces to match clap subcommands (`dependency-add`, `dependency-remove`, `assign-agent`, `phase`, `phases`, `worktree`, `confirm`).
- Corrected `ao task update --help` to only show implemented options and removed non-existent fields (`--type`, `--deadline`).
- Corrected `--input-json` modeling from file-path semantics to payload semantics (`--input-json <JSON>`) while preserving precedence guidance.
- Corrected `task-control set-deadline` format guidance to RFC3339.
- Synchronized `wireframes.html` and `cli-help-error-wireframe.tsx` help fixtures so deterministic test authorship can rely on one canonical set.

## State Coverage
- Help flow: `discovery`, `selection`, `ready`
- Validation flow: `error-shown`, `corrected-rerun`
- Destructive flow: `confirmation-blocked`, `preview-rendered`, `confirmed-rerun`
- JSON flow: `error-json`, `success-json`

## Canonical Contracts Modeled
- Invalid-value message:
  `invalid <domain> '<value>'; expected one of: <v1>, <v2>, ...; run the same command with --help`
- Confirmation-required message (non-git):
  `CONFIRMATION_REQUIRED: rerun '<command>' with --confirm <token>; use --dry-run to preview changes`
- Confirmation-required message (git):
  `CONFIRMATION_REQUIRED: request and approve a git confirmation for '<operation>' on '<repo>', then rerun with --confirmation-id <id>; use --dry-run to preview changes`
- Shared dry-run keys:
  `operation`, `target`, `action`, `destructive`, `dry_run`, `requires_confirmation`, `planned_effects`, `next_step`

## Accessibility and Responsive Intent
- Plain-text terminal-first rendering with no color-only meaning.
- Keyboard-visible focus states on all interactive controls.
- Message clauses kept deterministic and copy/paste safe (ASCII punctuation).
- Mobile board explicitly modeled at `320px`, with wrapped command lines and no horizontal scroll.
- Help and remediation copy keep explicit flag names (`--help`, `--dry-run`, `--confirm`, `--confirmation-id`).

## Acceptance Criteria Traceability

| AC | Trace |
| --- | --- |
| `AC-01` | Root/group help boards plus scoped group audit matrix preserve intent-first help hierarchy (`wireframes.html`, `ROOT_HELP_LINES`, `GROUP_HELP_LINES`, `*_HELP_LINES`) |
| `AC-02` | Command help board and scoped group audit snippets preserve argument clarity and accepted-value guidance (`wireframes.html`, `COMMAND_HELP_LINES`) |
| `AC-03` | `--input-json` precedence callouts in command help board and React scaffold (`wireframes.html`, `COMMAND_HELP_LINES`) |
| `AC-04` | Invalid-value board with deterministic accepted values + rerun hint (`wireframes.html`, `formatInvalidValueError`) |
| `AC-05` | Confirmation gate board with canonical task/workflow and git `CONFIRMATION_REQUIRED` variants (`wireframes.html`, `formatConfirmationRequired`, `formatGitConfirmationRequired`) |
| `AC-06` | Dry-run preview board includes shared key set in stable order (`wireframes.html`, `SHARED_DRY_RUN_KEYS`) |
| `AC-07` | JSON envelope examples preserve `ao.cli.v1` error/success semantics (`wireframes.html`, `CliErrorEnvelope`, `CliSuccessEnvelope`) |
| `AC-08` | Exit-code mapping shown in JSON error examples (`wireframes.html`) |
| `AC-09` | Destructive gate and dry-run-before-confirm sequence represented (`wireframes.html`, `DESTRUCTIVE_PREVIEW`) |
| `AC-10` | Deterministic help/error fixture strings are available for smoke/e2e assertion authoring (`ROOT_HELP_LINES`, `COMMAND_HELP_LINES`, formatter helpers) |
| `AC-11` | Canonical token order preserved in formatter helpers (`cli-help-error-wireframe.tsx`) |
| `AC-12` | No time/host-dependent phrases in static help/error templates (`wireframes.html`, `cli-help-error-wireframe.tsx`) |
