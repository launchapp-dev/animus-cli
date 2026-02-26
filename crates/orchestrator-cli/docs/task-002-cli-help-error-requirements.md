# TASK-002 Requirements: Audit and Polish CLI Help and Error Messages

## Phase
- Workflow phase: `requirements`
- Workflow ID: `4e289849-5501-4332-ba0d-e907038390ce`
- Task: `TASK-002`

## Objective
Define a deterministic, production-ready CLI UX contract for help output and
validation errors across core AO command groups so operators can:
- discover command intent quickly,
- understand argument formats and accepted values without reading source code,
- recover from invalid input with explicit next-step guidance.

## Existing Baseline Audit

| Area | Current state | Evidence | Gap |
| --- | --- | --- | --- |
| Help coverage | Top-level help title exists, but most commands/args rely on default clap rendering with no explicit help text | `crates/orchestrator-cli/src/cli_types.rs` (only root `#[command(...about...)]` metadata) | Operators do not get command intent, argument semantics, or usage guidance in help output |
| Enum-like argument clarity | Many bounded-domain args are plain `String` values parsed later | `crates/orchestrator-cli/src/shared/parsing.rs` and command args in `cli_types.rs` | Help output does not show accepted values; users learn only after failure |
| Validation message quality | Parse failures are terse (`invalid status: foo`, `invalid priority: foo`) | `parse_task_status`, `parse_priority_opt`, `parse_task_type_opt`, `parse_dependency_type`, `parse_project_type_opt` | Errors are not actionable: no accepted-values list and no remediation command hints |
| Confirmation error style | Similar destructive flows emit different guidance formats | `shared/parsing.rs::ensure_destructive_confirmation`, `ops_git/store.rs::ensure_confirmation` | Inconsistent wording and flag guidance across command groups |
| Preview/output style | Dry-run payload keys differ between task/workflow/git handlers | `runtime_project_task/task.rs`, `ops_task_control.rs`, `ops_workflow.rs`, `ops_git/repo.rs`, `ops_git/worktree.rs` | Cross-command automation and operator mental model are inconsistent |
| Help test coverage | Existing smoke coverage only checks top-level `--help` title/usage | `crates/orchestrator-cli/tests/cli_smoke.rs` | No regression guard for subcommand help clarity and argument descriptions |

## Scope
In scope for implementation after this requirements phase:
- Add explicit help metadata for core command groups:
  - `task`
  - `task-control`
  - `workflow`
  - `requirements`
  - `git`
  - global/root options
- Define and enforce a consistent validation error contract for invalid enum-like
  or malformed argument values.
- Align destructive confirmation guidance text and dry-run preview payload
  structure across task/workflow/git handlers.
- Add tests that lock expected help text presence and actionable validation
  messages.

### Scoped command matrix

| Command group | In-scope surfaces | Required outcome |
| --- | --- | --- |
| `task` | lifecycle and mutation commands with enum-like args (`status`, `priority`, `type`) and destructive paths | explicit help text, accepted-value visibility, actionable errors, consistent confirmation guidance |
| `task-control` | pause/resume/cancel/priority/deadline commands | explicit help text and deterministic validation guidance for malformed/invalid values |
| `workflow` | run/resume/cancel/phases and config-related commands | explicit intent-focused help and consistent confirmation/dry-run guidance for destructive operations |
| `requirements` | create/update/list/get and status/priority inputs | clear bounded-value guidance in help and parse-time errors |
| `git` | repo/worktree operations with confirmation and force semantics | consistent confirmation-required wording and dry-run preview key contract |
| root/global | top-level options and shared `--json` behavior | consistent wording for output mode expectations and input-json precedence |

Out of scope for this task:
- Adding new command families or renaming existing commands/flags.
- Changing `.ao` state schema or persistence behavior.
- Changing core domain semantics for tasks/workflows/git operations.
- Introducing interactive wizard flows beyond existing CLI behavior.
- Reworking command execution order, business rules, or side-effect timing.

## Constraints
- Preserve `ao.cli.v1` envelope behavior for `--json` responses.
- Preserve exit-code mapping contract in `shared/output.rs`:
  - `2` invalid input
  - `3` not found
  - `4` conflict
  - `5` unavailable
  - `1` internal
- Preserve existing accepted aliases where currently supported (for example
  `in-progress` and `in_progress`).
- Keep dry-run operations side-effect free.
- Keep changes scoped to `orchestrator-cli` docs/tests/handler UX behavior.
- Keep message text deterministic and free of environment-specific content.
- Preserve compatibility for existing automation that parses stable JSON fields.

## Functional Requirements

### FR-01: Command and Argument Help Metadata
- Core command groups must include explicit `about` text describing intent.
- User-facing arguments in scoped command groups must include concise help text
  that clarifies:
  - expected value format,
  - default behavior,
  - side-effect impact for destructive switches.
- `--input-json` flags must document precedence relative to individual flags.

### FR-02: Accepted Value Visibility
- For bounded-domain args (status, priority, task type, dependency type,
  project type, requirement status/priority), help output and/or argument parsing
  errors must clearly present accepted values.
- Alias forms that remain supported must be discoverable.
- Accepted values must be presented in deterministic order.

### FR-03: Actionable Validation Errors
- Invalid-value errors must include:
  - the argument or domain name,
  - the invalid value,
  - accepted values,
  - a next-step hint (`--help` or concrete rerun guidance).
- Missing-required input errors must identify the required flag and expected
  format.
- Error punctuation and phrasing must be stable across runs to support snapshots.

### FR-04: Confirmation Guidance Consistency
- Destructive flows must continue to emit `CONFIRMATION_REQUIRED`.
- Confirmation-required messages must include:
  - the required confirmation flag name (`--confirm` or `--confirmation-id`),
  - the expected token/approval source,
  - `--dry-run` guidance when available.

### FR-05: Dry-Run Preview Output Consistency
- Dry-run payloads for destructive task/workflow/git operations must expose a
  stable common shape:
  - `operation`
  - `target`
  - `destructive`
  - `dry_run`
  - `requires_confirmation`
  - `planned_effects`
  - `next_step`
- Command-specific details can be included, but common keys must remain stable.

### FR-06: Human and Machine Error Style Alignment
- Non-JSON mode errors must remain concise but actionable.
- JSON-mode error payloads must preserve current envelope shape while carrying
  improved message text.
- Error wording should be deterministic to avoid flaky CLI tests.

### FR-07: Regression Coverage
- Add/extend tests to verify:
  - help output includes new command/argument guidance,
  - invalid-value errors include accepted values and remediation hints,
  - confirmation-required wording stays consistent across scoped destructive
    commands,
  - dry-run preview payloads include the shared key set in stable form.

## Canonical Message Shapes

### Invalid value
- `invalid <domain> '<value>'; expected one of: <v1>, <v2>, ...; run '<command> --help'`

### Confirmation required
- `CONFIRMATION_REQUIRED: rerun '<command>' with <confirmation flag> <token>; use --dry-run to preview changes`

These are canonical message shapes for deterministic testing. Command-specific
details can vary, but key tokens and ordering must remain stable.

## Non-Functional Requirements

### NFR-01: Determinism
- Help and error text must be deterministic and testable.
- No time-dependent or environment-dependent phrasing in static help/error paths.

### NFR-02: Backward Compatibility
- Existing command invocation patterns remain valid.
- Existing JSON envelope fields remain unchanged.

### NFR-03: Operator Efficiency
- Operators should resolve common invalid-input failures in a single rerun
  without opening source code.

## Acceptance Criteria
- `AC-01`: Scoped command groups expose explicit `about` text in help output.
- `AC-02`: Key arguments in scoped groups expose concise help text with format
  and defaults.
- `AC-03`: `--input-json` help explicitly states precedence behavior.
- `AC-04`: Invalid status/priority/task-type/dependency/project-type values
  report accepted values and a remediation hint.
- `AC-05`: Confirmation-required errors across task/workflow/git include
  deterministic `CONFIRMATION_REQUIRED` and clear rerun guidance.
- `AC-06`: Dry-run payloads for scoped destructive operations expose the shared
  key set (`operation`, `target`, `dry_run`, etc.).
- `AC-07`: JSON mode retains `ao.cli.v1` envelope shape for success and errors.
- `AC-08`: Exit code mapping remains unchanged.
- `AC-09`: Existing destructive safety behavior (confirmation gating and dry-run
  no-mutation guarantee) remains intact.
- `AC-10`: New/updated tests cover help-text presence, validation message
  clarity, and confirmation guidance consistency.
- `AC-11`: Invalid-value and confirmation-required messages preserve canonical
  token order required for deterministic assertions.
- `AC-12`: No changes introduce non-deterministic text fragments (timestamps,
  absolute temp paths, host-specific details) in static help/error output.

## Verification Matrix

| Requirement | Verification method |
| --- | --- |
| Help metadata coverage | CLI smoke tests asserting scoped command help content |
| Accepted-value visibility | Unit tests for parsing helpers and/or clap parser errors |
| Actionable validation text | Assertions on error payload message content in CLI tests |
| Confirmation guidance consistency | E2E tests for task/workflow/git destructive commands |
| Dry-run preview key stability | JSON assertions for preview payload key set |
| Envelope + exit-code compatibility | Existing envelope tests + exit-code regression tests |
| Canonical token ordering | Snapshot/assertion tests for canonical invalid-value and confirmation-required shapes |

## Deterministic Deliverables for Implementation Phase
- Updated command/argument help metadata in `cli_types.rs`.
- Shared validation error formatting for bounded-domain parsers.
- Aligned confirmation-required messaging contract across task/workflow/git.
- Standardized dry-run preview payload keys for scoped destructive commands.
- Expanded CLI tests for help content and actionable validation errors.
