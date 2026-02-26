# TASK-002 Requirements: Audit and Polish CLI Help and Error Messages

## Phase
- Workflow phase: `requirements`
- Workflow ID: `c280da10-e502-499b-9f57-62e75e158630`
- Task: `TASK-002`

## Objective
Define a deterministic, production-ready CLI UX contract for help output and
validation errors across core AO command groups so operators can:
- discover command intent quickly,
- understand argument formats and accepted values without reading source code,
- recover from invalid input with explicit next-step guidance,
- execute destructive operations safely with predictable preview and
  confirmation flows.

## Audit Snapshot (Current Baseline)

| Area | Current state | Evidence | Gap |
| --- | --- | --- | --- |
| Help metadata | Command groups and many high-impact flags now include explicit help text and bounded-value hints | `crates/orchestrator-cli/src/cli_types.rs` | Need final audit to ensure scoped groups stay consistent after future edits |
| Bounded-domain parse errors | Task/project/requirement parsers already emit accepted values + `--help` hint | `crates/orchestrator-cli/src/shared/parsing.rs`, `crates/orchestrator-cli/src/services/operations/ops_requirements/state.rs` | Need canonical message contract alignment across all bounded domains |
| Confirmation-required wording | Non-git destructive flows use `--confirm`; git flows use `--confirmation-id` with different phrasing | `shared/parsing.rs::ensure_destructive_confirmation`, `ops_git/store.rs::ensure_confirmation` | Canonical token order and remediation wording are not unified |
| Dry-run payload shape | Destructive previews already include shared fields plus command-specific fields | `runtime_project_task/task.rs`, `ops_task_control.rs`, `ops_workflow.rs`, `ops_git/repo.rs`, `ops_git/worktree.rs` | Shared key contract must be explicitly locked with tests |
| Regression coverage | `cli_smoke` and `cli_e2e` already cover many help/error cases | `crates/orchestrator-cli/tests/cli_smoke.rs`, `crates/orchestrator-cli/tests/cli_e2e.rs` | Missing explicit assertions for canonical message token order and shared dry-run key set across all scoped destructive paths |

## Scope
In scope for implementation after this requirements phase:
- Audit and finalize help/output consistency for these command groups:
  - `task`
  - `task-control`
  - `workflow`
  - `requirements`
  - `git`
  - root/global options (`--json`, `--project-root`)
- Normalize canonical error contracts for:
  - invalid bounded-domain values,
  - destructive confirmation-required flows.
- Lock a stable dry-run preview schema for destructive task/workflow/git
  operations.
- Extend deterministic tests for message contract and dry-run key contract.

### Scoped command matrix

| Command group | In-scope surfaces | Required outcome |
| --- | --- | --- |
| `task` | mutation commands (`update`, `status`, `delete`) and bounded value parsing | deterministic help/value guidance and canonical invalid-value/confirmation errors |
| `task-control` | `cancel`, `set-priority`, `set-deadline` | deterministic validation guidance and confirmation/dry-run messaging |
| `workflow` | destructive commands (`pause`, `cancel`, `phases remove`) | canonical confirmation-required wording and stable preview schema |
| `requirements` | `create`, `update`, list filters with bounded value args | accepted value visibility + canonical invalid requirement value errors |
| `git` | `repo push`, `worktree remove/push` destructive flows | explicit `--confirmation-id` guidance and stable dry-run schema |
| root/global | `ao --help`, global options | stable wording for output mode and root resolution controls |

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
- Do not manually edit `/.ao/*.json` files.

## Functional Requirements

### FR-01: Command and Argument Help Metadata
- Scoped command groups must expose explicit intent-first help text.
- High-impact user-facing arguments in scoped groups must include concise help
  text that clarifies:
  - expected value format,
  - default behavior,
  - side-effect impact for destructive switches.
- `--input-json` flags must document precedence relative to individual flags.

### FR-02: Accepted Value Visibility
- For bounded-domain args (status, priority, task type, dependency type, project
  type, requirement status/priority), help output and/or parse-time errors must
  clearly present accepted values.
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
- Canonical invalid-value contract for shared parsing helpers:
  - `invalid <domain> '<value>'; expected one of: <v1>, <v2>, ...; run the same command with --help`

### FR-04: Confirmation Guidance Consistency
- Destructive flows must continue to emit `CONFIRMATION_REQUIRED`.
- Confirmation-required messages must include:
  - the required confirmation flag name (`--confirm` or `--confirmation-id`),
  - the expected token/approval source,
  - `--dry-run` guidance when available.
- Canonical token order must be stable per confirmation flow type:
  - non-git: `rerun '<command>' with --confirm <token>; use --dry-run ...`
  - git: `request and approve ...; rerun with --confirmation-id <id>; use --dry-run ...`

### FR-05: Dry-Run Preview Output Consistency
- Dry-run payloads for destructive task/workflow/git operations must expose a
  stable common shape:
  - `operation`
  - `target`
  - `action`
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
  - invalid-value errors include accepted values and remediation hints with
    canonical token order,
  - confirmation-required wording stays consistent across scoped destructive
    commands,
  - dry-run preview payloads include the shared key set in stable form.

## Canonical Message Shapes

### Invalid value
- `invalid <domain> '<value>'; expected one of: <v1>, <v2>, ...; run the same command with --help`

### Confirmation required
- Non-git: `CONFIRMATION_REQUIRED: rerun '<command>' with --confirm <token>; use --dry-run to preview changes`
- Git: `CONFIRMATION_REQUIRED: request and approve a git confirmation for '<operation>' on '<repo>', then rerun with --confirmation-id <id>; use --dry-run to preview changes`

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
- `AC-01`: Scoped command groups retain explicit intent text in help output.
- `AC-02`: Key arguments in scoped groups retain concise help text with format
  and default/side-effect guidance.
- `AC-03`: `--input-json` help explicitly states precedence behavior.
- `AC-04`: Invalid status/priority/task-type/dependency/project-type/requirement
  values report accepted values and a remediation hint.
- `AC-05`: Confirmation-required errors across task/workflow and git include
  deterministic `CONFIRMATION_REQUIRED`, explicit confirmation flag guidance,
  and stable token order.
- `AC-06`: Dry-run payloads for scoped destructive operations expose the shared
  key set (`operation`, `target`, `action`, `dry_run`, etc.).
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
| Accepted-value visibility | Unit tests for parsing helpers and requirements state parsers |
| Actionable validation text | Assertions on error payload message content in CLI tests |
| Confirmation guidance consistency | E2E tests for task/workflow/git destructive commands |
| Dry-run preview key stability | JSON assertions for preview payload key set |
| Envelope + exit-code compatibility | Existing envelope tests + exit-code regression tests |
| Canonical token ordering | Snapshot/assertion tests for canonical invalid-value and confirmation-required shapes |

## Deterministic Deliverables for Implementation Phase
- Finalized help copy audit adjustments in `cli_types.rs` where drift exists.
- Canonical validation/confirmation message contract alignment where drift exists.
- Standardized dry-run preview payload key contract for scoped destructive commands.
- Expanded CLI tests for canonical messaging and preview key stability.
