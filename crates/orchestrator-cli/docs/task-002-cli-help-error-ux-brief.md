# TASK-002 UX Brief: CLI Help and Error Message Experience

## Phase
- Workflow phase: `ux-research`
- Workflow ID: `c280da10-e502-499b-9f57-62e75e158630`
- Task: `TASK-002`

## Inputs
- Requirements baseline: `crates/orchestrator-cli/docs/task-002-cli-help-error-requirements.md`
- Scoped command groups: `task`, `task-control`, `workflow`, `requirements`, `git`, root/global options
- Message contracts to preserve: `ao.cli.v1` envelope, canonical invalid-value shape, canonical confirmation-required shape

## UX Objective
Define a deterministic CLI guidance experience that lets operators and automation:
- discover command intent quickly,
- supply valid arguments without source lookup,
- recover from invalid input in one rerun,
- safely execute destructive flows via preview and explicit confirmation.

## Primary Users
| User | Primary job | UX success signal |
| --- | --- | --- |
| Operator | Find correct command/flags for task/workflow/git operations | Successful invocation after help lookup in one navigation path |
| Automation engineer | Parse failures and route retries in CI scripts | Deterministic branch logic from `ao.cli.v1` and stable message tokens |
| Reviewer/on-call | Triage command failures and guide rerun | Can copy/paste exact remediation command from output |

## Key Screens (CLI Surfaces)
| Screen ID | Surface | User goal | Required hierarchy |
| --- | --- | --- | --- |
| S1 | Root help (`ao --help`) | Discover top-level capability map | intent -> command groups -> global options |
| S2 | Group help (`ao <group> --help`) | Choose correct subcommand | group intent -> subcommands -> shared options |
| S3 | Command help (`ao <group> <command> --help`) | Build valid invocation | intent -> usage -> options/args -> accepted values/precedence |
| S4 | Invalid-value error | Repair bounded-domain input | failing domain/value -> accepted values -> rerun hint |
| S5 | Confirmation-required error | Complete destructive flow safely | `CONFIRMATION_REQUIRED` -> exact confirmation guidance -> `--dry-run` hint |
| S6 | Destructive dry-run preview | Review impact before mutation | operation summary -> target/action -> planned effects -> next step |
| S7 | JSON envelope (`--json`) | Machine-safe branching | `schema/ok` + success data OR `schema/ok=false/error` |

## Interaction and State Model
| Surface | Trigger | Primary interaction | State transition |
| --- | --- | --- | --- |
| S1 -> S2 -> S3 | User needs command shape | Drill down from root to group to command help | `help-loaded` -> `command-selected` -> `ready-to-run` |
| S4 | Invalid bounded value | Read accepted values and rerun with corrected token | `error-shown` -> `corrected-rerun` |
| S5 -> S6 -> execution | Destructive command without confirmation | Read confirmation requirement, optionally preview, rerun with confirmation token | `confirmation-blocked` -> `previewed` -> `confirmed-rerun` |
| S7 | Automation path | Parse deterministic error text and exit code mapping | `error-json` or `success-json` |

## Critical User Flows
### Flow A: Command discovery and first-run success
1. User runs `ao --help`.
2. User picks a command group and opens `ao <group> --help`.
3. User opens command-level help for the intended action.
4. User executes the command with valid args.

### Flow B: Invalid bounded value recovery
1. User runs a command with unsupported `status`/`priority`/`type`/other bounded value.
2. CLI returns canonical invalid-value message.
3. User reruns once using accepted values from the message or `--help`.

### Flow C: Destructive safety gate
1. User invokes destructive action without confirmation material.
2. CLI returns `CONFIRMATION_REQUIRED` with exact flag guidance (`--confirm` or `--confirmation-id`).
3. User may run `--dry-run` to inspect `planned_effects`.
4. User reruns with required confirmation input; mutation proceeds only then.

### Flow D: JSON automation handling
1. Automation calls command with `--json`.
2. On validation/confirmation failure, CLI emits `ao.cli.v1` error envelope.
3. Script branches on `ok`, `error.code`, and deterministic message contract.

## Responsive Terminal Behavior
- `>=100 cols`: keep canonical error shapes on one line where possible.
- `80-99 cols`: wrap only at clause separators (`;`) while preserving clause order.
- `<80 cols`: keep clause order unchanged; avoid splitting critical tokens (`--confirm`, `--confirmation-id`, command names).

## Accessibility Constraints (Non-Negotiable)
1. No color-only meaning; output must be complete in plain text.
2. Keep copy ASCII-safe and terminal-safe for reliable paste and screen readers.
3. Preserve deterministic token order for assistive parsing and snapshot tests.
4. Require keyboard-only flows; no interactive prompt dependency for core recovery path.
5. Keep remediation explicit with full flag names (`--help`, `--dry-run`, `--confirm`, `--confirmation-id`).
6. Avoid control characters or formatting that breaks high-contrast/low-vision terminal themes.
7. Maintain machine-parseable JSON output with no extra human-only prefixes.

## Deterministic Content Contracts
### Invalid-value contract
`invalid <domain> '<value>'; expected one of: <v1>, <v2>, ...; run the same command with --help`

### Confirmation-required contract
- Non-git: `CONFIRMATION_REQUIRED: rerun '<command>' with --confirm <token>; use --dry-run to preview changes`
- Git: `CONFIRMATION_REQUIRED: request and approve a git confirmation for '<operation>' on '<repo>', then rerun with --confirmation-id <id>; use --dry-run to preview changes`

### Dry-run preview contract
Shared keys must be present in stable order:
1. `operation`
2. `target`
3. `action`
4. `destructive`
5. `dry_run`
6. `requires_confirmation`
7. `planned_effects`
8. `next_step`

## Requirements Traceability
| Requirement group | UX surfaces |
| --- | --- |
| FR-01 help metadata | S1, S2, S3 |
| FR-02 accepted values | S3, S4 |
| FR-03 actionable validation errors | S4, S7 |
| FR-04 confirmation guidance | S5, S6, S7 |
| FR-05 dry-run schema consistency | S6, S7 |
| FR-06 human/machine alignment | S4, S5, S7 |
| FR-07 regression coverage targets | S1-S7 |

## Implementation Handoff Checklist
- Keep command/group help copy intent-first and precedence-aware (`--input-json` where supported).
- Preserve canonical invalid-value and confirmation-required token order.
- Keep dry-run shared key set stable across task/workflow/git destructive paths.
- Assert both plain-text and `--json` surfaces in smoke/e2e tests.
