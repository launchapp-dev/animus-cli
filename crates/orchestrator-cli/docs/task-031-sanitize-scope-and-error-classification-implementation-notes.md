# TASK-031 Implementation Notes: Consolidate Repository Scope and Error Classification Helpers

## Purpose
Provide an implementation-ready plan for TASK-031 that removes duplicated
helper logic and enforces deterministic cross-crate behavior.

## Locked Decisions
- Shared location is fixed to `crates/protocol` (not optional).
- Repository slug derivation is standardized to canonicalized path basename.
- Classifier precedence is fixed:
  `invalid_input -> not_found -> conflict -> unavailable -> internal`.
- CLI and web-contracts keep their existing public wrapper signatures.

## Non-Negotiable Constraints
- Keep changes scoped to helper consolidation and related tests.
- Preserve CLI envelope (`ao.cli.v1`) and existing exit-code mapping values.
- Preserve repository scope hash strategy and output shape.
- Do not manually edit `/.ao/*.json`.
- Keep dependency graph acyclic.

## Planned Change Surface

### 1) Add canonical protocol helper modules
- Add `crates/protocol/src/repository_scope.rs` with:
  - `pub fn sanitize_identifier(value: &str) -> String`
  - `pub fn repository_scope_for_path(path: &Path) -> String`
- Add `crates/protocol/src/error_classification.rs` with:
  - `pub fn classify_error_message(message: &str) -> (&'static str, i32)`
- Update `crates/protocol/src/lib.rs` to export both modules/functions.
- Add `sha2` dependency to `crates/protocol/Cargo.toml`.

### 2) Repository scope call-site consolidation
- `crates/orchestrator-core/src/services.rs`
  - remove local `sanitize_identifier` and `repository_scope_for_path`.
  - switch `index_root_for_state_file` to protocol helper.
- `crates/orchestrator-core/src/services/runner_helpers.rs`
  - remove local sanitize/scope helpers.
  - switch `project_runtime_root` path scope derivation to protocol helper.
- `crates/orchestrator-cli/src/shared/runner.rs`
  - remove local sanitize/scope helpers.
  - switch `scoped_ao_root` to protocol helper.
- `crates/orchestrator-core/src/services/tests.rs`
  - remove copied helper algorithm and assert via protocol helper behavior.

### 3) Error classifier consolidation
- `crates/orchestrator-cli/src/shared/output.rs`
  - keep `classify_error(&anyhow::Error)` adapter.
  - delegate pattern matching to `protocol::classify_error_message`.
  - keep `should_emit_help_hint` unchanged.
- `crates/orchestrator-web-contracts/src/utils/classify_error.rs`
  - keep `classify_error(&str)` wrapper.
  - delegate to `protocol::classify_error_message`.
- `crates/orchestrator-web-contracts/Cargo.toml`
  - add `protocol = { path = "../protocol" }`.
- `crates/orchestrator-web-api/src/models/web_api_error.rs`
  - no behavior change expected; only adjust imports if required.

## Deterministic Behavior Rules To Preserve
- Repository scope output remains `<slug>-<12hex>`.
- Slug normalization remains:
  - lowercase alphanumeric characters kept
  - spaces/`_`/`-` mapped to `-`
  - repeated `-` collapsed
  - leading/trailing `-` trimmed
  - fallback `"repo"` when empty
- Hash remains first six SHA-256 bytes of canonical path string bytes.
- Classifier code/exit mapping remains:
  - `invalid_input` => `2`
  - `not_found` => `3`
  - `conflict` => `4`
  - `unavailable` => `5`
  - fallback `internal` => `1`

## Explicitly Out of Scope
- Changing sanitizers unrelated to repository scope (`sanitize_identifier_for_git`,
  daemon notification path sanitizers, etc.).
- Changing CLI/Web envelope schema.
- Changing daemon lifecycle, workflow scheduling, or task-control semantics.

## Implementation Sequence
1. Add protocol helper modules and protocol unit tests.
2. Refactor repository scope call sites in core/cli.
3. Remove copied repository scope algorithm from core tests.
4. Refactor CLI/web-contracts classifier adapters to protocol helper.
5. Add/adjust parity regression tests in protocol/cli/web-contracts.
6. Run targeted tests and fix only regressions introduced by this change.

## Risks and Mitigations
- Risk: scope drift from raw-path vs canonical-basename differences.
  - Mitigation: codify canonical-basename behavior and pin with tests.
- Risk: classifier overlap regressions due to pattern ordering.
  - Mitigation: lock and test precedence order.
- Risk: added dependency surface in web-contracts.
  - Mitigation: keep shared helpers lightweight and pure utility logic.

## Validation Targets
- `cargo test -p protocol`
- `cargo test -p orchestrator-core services::tests`
- `cargo test -p orchestrator-cli shared::output`
- `cargo test -p orchestrator-web-contracts`
- `cargo test -p orchestrator-web-api`
