# TASK-034 Implementation Notes: Typed CLI Error Classification

## Phase Context
- Workflow phase: `requirements`
- Workflow ID: `4228561a-d7da-4a6c-833c-32693f659e41`
- Task: `TASK-034`
- Requirement: `REQ-002`

## Purpose
Translate `TASK-034` into a deterministic implementation slice that replaces
message-based exit-code classification with typed errors while preserving
current CLI output and exit semantics.

## Non-Negotiable Constraints
- Keep `ao.cli.v1` success/error envelope shape unchanged.
- Keep numeric exit-code contract unchanged (`2/3/4/5/1`).
- Keep changes scoped to `orchestrator-cli` in this task.
- Do not classify by message substrings in `shared/output.rs`.
- Do not manually edit `.ao/*.json`.

## Proposed Change Surface

### 1) Add a typed CLI classification model
- Add a new shared module (recommended):
  - `crates/orchestrator-cli/src/shared/cli_error.rs`
- Define:
  - `CliErrorKind` enum for `InvalidInput`, `NotFound`, `Conflict`,
    `Unavailable`, `Internal`.
  - deterministic mapping methods from kind to `(code, exit_code)`.
  - typed error wrapper (for `anyhow` interop) carrying `kind + message`.
  - constructor helpers for each kind (`invalid_input`, `not_found`, etc.).

Recommended design target:
- keep the type minimal and `std::error::Error` compatible.
- avoid dynamic message parsing in classifier.
- make kind-to-code mapping a single source of truth.

### 2) Wire the shared module into exports
- Update:
  - `crates/orchestrator-cli/src/shared.rs`
- Export the new typed error model/helpers so existing service modules can use
  them without deep path coupling.

### 3) Replace classifier internals in `output.rs`
- Update:
  - `crates/orchestrator-cli/src/shared/output.rs`
- Replace string-pattern arrays and substring checks with typed classification:
  - downcast/chain-inspect `anyhow::Error` for typed CLI error values.
  - map typed kind directly to `(code, exit_code)`.
  - fallback to `internal` for untyped/unclassified errors.

Optional compatibility guard (if needed):
- classify typed `std::io::ErrorKind` cases (`NotFound`, selected connection or
  timeout kinds) only if necessary to preserve legacy behavior for unwrapped IO
  failures.

### 4) Migrate high-value error producers to typed constructors
- Prioritize current surfaces that intentionally rely on message text:
  - `crates/orchestrator-cli/src/shared/parsing.rs`
  - representative not-found/conflict sites in
    `crates/orchestrator-cli/src/services/operations/`
  - representative unavailable sites in
    `crates/orchestrator-cli/src/shared/runner.rs` and runtime handlers.

Migration rule:
- keep existing message body, but construct error through typed helper.
- avoid broad behavioral edits unrelated to classification.

### 5) Preserve emission contract in `emit_cli_error`
- `emit_cli_error` stays behavior-compatible:
  - JSON envelope shape unchanged.
  - invalid-input `--help` hint remains tied to invalid-input kind.
  - non-invalid classes do not emit hint.

### 6) Expand and tighten tests
- Update/add tests in:
  - `crates/orchestrator-cli/src/shared/output.rs`
  - `crates/orchestrator-cli/src/shared.rs`
  - `crates/orchestrator-cli/src/shared/parsing.rs`
- Target assertions:
  - all five typed kinds map to expected codes.
  - same kind + different message -> same exit code.
  - untyped errors map to internal.
  - JSON envelope + non-JSON hint behavior remains stable.

## Suggested Implementation Sequence
1. Add `cli_error.rs` and shared exports.
2. Refactor `output.rs` classifier to typed classification only.
3. Migrate parsing helpers to typed invalid-input constructors.
4. Migrate selected not-found/conflict/unavailable call sites.
5. Update tests for typed behavior and compatibility invariants.
6. Run targeted test commands and fix regressions introduced by this task.

## Validation Targets
- `cargo test -p orchestrator-cli shared::output`
- `cargo test -p orchestrator-cli shared::parsing`
- `cargo test -p orchestrator-cli classify_error_maps_expected_exit_codes`
- `cargo test -p orchestrator-cli --test cli_smoke`

## Risks and Mitigations
- Risk: incomplete call-site migration changes observed exit codes.
  - Mitigation: prioritize parsing + high-traffic not-found/conflict paths first
    and add explicit regression tests.
- Risk: typed classifier fails to detect wrapped errors.
  - Mitigation: inspect full `anyhow` error chain for typed value.
- Risk: accidental schema drift in error envelope.
  - Mitigation: keep envelope structs unchanged; add JSON snapshot-style asserts.
- Risk: scope creep into web/API contracts.
  - Mitigation: keep task slice limited to `orchestrator-cli`.

## Out-of-Scope Reminder
- `crates/orchestrator-web-contracts/src/utils/classify_error.rs` is not part of
  this task’s implementation slice.
- Cross-crate deduplication initiatives can be handled separately.
