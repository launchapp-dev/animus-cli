# TASK-002 UX Brief: CLI Help and Error Message Experience

## Phase
- Workflow phase: `ux-research`
- Workflow ID: `4e289849-5501-4332-ba0d-e907038390ce`
- Task: `TASK-002`

## UX Objective
Define a deterministic CLI guidance experience that helps operators:
- discover command intent quickly,
- choose valid arguments without source-code lookup,
- recover from invalid input in one rerun,
- safely execute destructive flows with explicit preview and confirmation.

The UX must work consistently across human terminal use and machine-driven
`--json` automation.

## Primary Users and Jobs

| User | Primary jobs | UX success signal |
| --- | --- | --- |
| Operator | Discover command shape and run task/workflow/git operations correctly | Finds correct command + args from help in under 30 seconds |
| Automation engineer | Parse deterministic errors and dry-run previews in CI scripts | Can route invalid-input failures without string-guessing |
| Reviewer/on-call | Triage user command failures and provide exact rerun guidance | Can copy/paste remediation command from error output directly |

## UX Principles
1. Intent first: every help surface explains what the command does before flags.
2. Recovery first: invalid input output always tells users exactly how to rerun.
3. Deterministic language: token order and punctuation remain stable for tests.
4. Safety before mutation: destructive paths always advertise dry-run + confirmation.
5. Human/machine parity: non-JSON and JSON outputs carry the same actionable meaning.

## Information Architecture

### Primary CLI Surfaces ("Screens")
1. Root help surface: `ao --help`
2. Command-group help surface: `ao task --help`, `ao workflow --help`, `ao requirements --help`, `ao task-control --help`, `ao git --help`
3. Command-level help surface: `ao <group> <command> --help`
4. Invalid-value error surface: bounded-domain parse failures (`status`, `priority`, `type`, etc.)
5. Confirmation-required error surface: destructive command run without required confirmation token
6. Destructive dry-run preview surface: side-effect-free summary before live execution
7. JSON envelope surface: `ao.cli.v1` success/error payloads for automation

### Content Hierarchy Per Surface
1. Command intent (`about` text)
2. Usage line
3. Primary options and expected formats
4. Accepted values or alias guidance
5. Next-step examples (`--help`, `--dry-run`, confirmation flag)

## Key Screen and Interaction Contracts

| Surface | User goal | Primary interactions | Required states |
| --- | --- | --- | --- |
| Root help | Understand available command groups quickly | Read summary, select group | help-loaded |
| Group help | Identify right subcommand and shared flags | Scan subcommands, inspect key options | help-loaded |
| Command help | Build valid invocation first try | Inspect value format/defaults/precedence | help-loaded |
| Invalid-value error | Fix malformed or unsupported input | Read invalid value + accepted list + rerun hint | error-shown, corrected-rerun |
| Confirmation-required error | Complete destructive action safely | Obtain/provide confirmation token or run `--dry-run` | confirmation-blocked, previewed, confirmed-rerun |
| Dry-run preview | Understand impact before mutation | Review deterministic key set and planned effects | preview-rendered |
| JSON envelope | Integrate with scripts/tools | Parse `ok/error`, branch on message code/text | success-json, error-json |

## Critical User Flows

### Flow A: Discover and Run a Command
1. User runs `ao --help`.
2. User chooses command group from grouped help content.
3. User runs `ao <group> --help` and then command-level `--help`.
4. User sees argument format and accepted value guidance.
5. User executes command successfully on first full attempt.

### Flow B: Invalid Bounded Value Recovery
1. User runs command with invalid value (example: unsupported status).
2. CLI returns canonical invalid-value message with invalid token preserved.
3. Message includes deterministic accepted-values list and `--help` rerun hint.
4. User reruns once with valid value and proceeds.

### Flow C: Destructive Operation Safety Gate
1. User starts destructive command without confirmation material.
2. CLI returns `CONFIRMATION_REQUIRED` with exact flag/token guidance.
3. User optionally runs `--dry-run` to inspect planned effects.
4. User reruns with required confirmation input.
5. Operation executes only after explicit confirmation.

### Flow D: Machine-Mode Error Handling
1. Automation invokes command with `--json`.
2. On invalid input, CLI returns `ao.cli.v1` error envelope.
3. Automation reads deterministic message text and exits with mapped code.
4. Script emits remediation or retries with corrected args.

## Layout, Hierarchy, and Responsive Terminal Behavior

### Terminal Width Strategy
- `>=100 cols`: keep canonical messages on one line where possible.
- `80-99 cols`: allow natural wrapping at separators (`;`) without losing order.
- `<80 cols`: keep clauses short and front-load actionable guidance.

### Help Readability Rules
- Keep section order stable: intent -> usage -> args/options -> examples/next step.
- Keep argument descriptions concise and format-specific.
- Keep accepted values in deterministic, comma-separated order.

### Error Readability Rules
- Start with failure domain and offending value.
- Follow with accepted values.
- End with concrete next-step command hint.

## Accessibility Constraints (Non-Negotiable)
1. Do not rely on color to convey required meaning.
2. Keep all help/error content meaningful in plain-text terminals.
3. Use ASCII-safe punctuation and quoting for reliable copy/paste and TTY support.
4. Preserve deterministic phrase order for screen readers and assistive parsing.
5. Ensure guidance is keyboard-only operable (no interactive prompt dependency).
6. Keep command hints explicit and fully typed (no implied placeholders only).
7. Avoid control characters or formatting that degrade in low-vision/high-contrast terminal themes.
8. Maintain parseable JSON mode output with no extra human-only prefixes.
9. Keep line wrapping tolerant of narrow terminals without truncating critical tokens.
10. Include explicit flag names in remediation messages (`--help`, `--dry-run`, `--confirm`, `--confirmation-id`).

## Content Contract for Implementation Phase

### Invalid-Value Message Contract
- Shape: `invalid <domain> '<value>'; expected one of: <v1>, <v2>, ...; run '<command> --help'`
- Requirements:
  - deterministic accepted-value order,
  - stable punctuation and clause order,
  - includes actionable rerun hint.

### Confirmation-Required Message Contract
- Shape: `CONFIRMATION_REQUIRED: rerun '<command>' with <confirmation flag> <token>; use --dry-run to preview changes`
- Requirements:
  - include exact required confirmation flag,
  - include preview guidance when supported,
  - remain stable for snapshot assertions.

### Dry-Run Preview UX Contract
Top-level keys must be present and stable:
- `operation`
- `target`
- `destructive`
- `dry_run`
- `requires_confirmation`
- `planned_effects`
- `next_step`

## UX Acceptance Checklist for Implementation
- Root and scoped command help expose clear command intent (`about`) text.
- Key arguments include format/accepted-value guidance.
- `--input-json` precedence is explicitly documented where applicable.
- Invalid-value errors include domain, invalid value, accepted values, and rerun hint.
- Destructive commands emit consistent `CONFIRMATION_REQUIRED` guidance.
- Dry-run previews expose shared key set in stable order.
- Non-JSON and JSON surfaces remain behaviorally aligned and deterministic.
- Output remains legible and actionable on narrow terminals.
