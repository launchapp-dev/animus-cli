# TASK-034 Requirements: Typed CLI Error Classification for Exit-Code Safety

## Phase
- Workflow phase: `requirements`
- Workflow ID: `4228561a-d7da-4a6c-833c-32693f659e41`
- Task: `TASK-034`
- Requirement: `REQ-002`

## Objective
Replace string-substring based exit-code classification in `orchestrator-cli`
with typed error kinds so exit semantics are compile-time safe and stable even
when human-readable error messages change.

## Current Baseline (Implemented)

| Surface | Current location | Current behavior | Gap |
| --- | --- | --- | --- |
| CLI error classification | `crates/orchestrator-cli/src/shared/output.rs` | `classify_error` lowercases `err.to_string()` and matches pattern lists (`not found`, `already`, `timeout`, etc.) | Exit code depends on mutable message text instead of typed semantics |
| Exit code dispatch | `crates/orchestrator-cli/src/main.rs` | process exit code is selected by `classify_exit_code(&error)` | message wording drift can change process exit behavior |
| Invalid-input builders | `crates/orchestrator-cli/src/shared/parsing.rs` | invalid-input failures are plain `anyhow!` strings (`invalid ...; expected one of ...`) | invalid-input classification currently relies on substring matching |
| Not-found/conflict domain errors | `crates/orchestrator-cli/src/services/operations/*` | many command handlers produce `anyhow!(\"... not found ...\")` / `anyhow!(\"... already exists ...\")` | classification contract is implicit in text and not checked by type system |
| Regression coverage | `crates/orchestrator-cli/src/shared/output.rs`, `crates/orchestrator-cli/src/shared.rs` | tests assert mapping by constructing message strings | tests validate text patterns, not typed error invariants |

## Scope
In scope for implementation after this requirements phase:
- introduce a typed error hierarchy for CLI classification in
  `orchestrator-cli`.
- make `classify_error` and `classify_exit_code` depend on typed error metadata,
  not message substring scans.
- migrate representative invalid/not-found/conflict/unavailable producers to
  typed constructors so current CLI semantics remain intact.
- preserve existing envelope shape and numeric exit-code contract.
- add deterministic tests proving message text does not control classification.

Out of scope for this task:
- changing the top-level `ao.cli.v1` JSON schema.
- changing command dispatch flow in `main.rs`.
- changing `orchestrator-web-contracts` classifier behavior in this task slice.
- broad cross-crate error-taxonomy unification outside `orchestrator-cli`.
- direct manual edits to `.ao/*.json`.

## Constraints
- Preserve current exit-code mapping contract:
  - `2` invalid input
  - `3` not found
  - `4` conflict
  - `5` unavailable
  - `1` internal
- Preserve non-JSON and JSON output shape in `emit_cli_error`.
- Error classification logic must not match on free-form message substrings.
- Keep behavior deterministic for both typed and untyped error paths.
- Keep implementation scoped to Rust crates in this repository.

## Functional Requirements

### FR-01: Typed Error Kind Model
- Define a first-class CLI error kind enum with exactly these variants:
  - `InvalidInput`
  - `NotFound`
  - `Conflict`
  - `Unavailable`
  - `Internal`
- Enum must provide deterministic mapping to:
  - machine error code string (`invalid_input`, `not_found`, etc.)
  - process exit code (`2`, `3`, `4`, `5`, `1`).

### FR-02: Typed CLI Error Payload
- Add a typed error value that carries:
  - error kind
  - human-readable message
- The value must implement `std::error::Error` and `Display` so it can be
  wrapped in `anyhow::Error`.

### FR-03: Constructor/Factory Surface
- Provide shared constructors/helpers to create typed errors for each class:
  - invalid input
  - not found
  - conflict
  - unavailable
  - internal
- Helpers must allow existing handler sites to keep contextual messages while
  explicitly setting classification kind.

### FR-04: Classifier Behavior
- `classify_error` must classify from typed metadata (downcast and/or error
  chain inspection), not message string matching.
- Pattern arrays and string-containment helper logic in `output.rs` must be
  removed.
- Unclassified/untyped errors must deterministically map to `internal` (exit
  code `1`).

### FR-05: Migration Coverage for Existing Semantics
- Migrate currently intentional category-producing surfaces to typed errors:
  - invalid-value parsing helpers in `shared/parsing.rs`
  - representative not-found/conflict handler returns in
    `services/operations/*`
  - representative availability failures in runner connection/status paths
    where current behavior is expected to be `unavailable`
- Migration must keep user-facing message content meaningful and actionable.

### FR-06: Output Contract Compatibility
- JSON error envelope remains:
  - `schema: "ao.cli.v1"`
  - `ok: false`
  - `error.code`
  - `error.message`
  - `error.exit_code`
- Non-JSON invalid-input hint behavior remains unchanged:
  - include `hint: run with --help ...` only for invalid-input classified
    errors.

### FR-07: Compile-Time Safety and Determinism
- Changing only error message text must not change exit-code classification when
  the typed kind is unchanged.
- Tests must assert this invariant explicitly.

### FR-08: Regression Coverage
- Add unit/integration tests that cover:
  - typed classification for all five error kinds
  - fallback of untyped errors to internal
  - message mutation does not change typed classification
  - `emit_cli_error` envelope and hint behavior with typed errors

## Acceptance Criteria
- `AC-01`: `output.rs::classify_error` contains no substring pattern lists for
  classification.
- `AC-02`: exit-code mapping remains `2/3/4/5/1` for
  invalid/not-found/conflict/unavailable/internal respectively.
- `AC-03`: typed errors are available as shared constructors and used by parsing
  and representative service call sites.
- `AC-04`: existing JSON envelope shape remains backward compatible.
- `AC-05`: non-JSON invalid-input hint remains present only for invalid-input
  errors.
- `AC-06`: message-only changes do not alter classification for typed errors.
- `AC-07`: untyped errors still classify deterministically as `internal`.
- `AC-08`: targeted tests pass for typed classification behavior and output
  contract compatibility.
- `AC-09`: no direct edits are made to `.ao/*.json`.

## Testable Acceptance Checklist
- `T-01`: unit tests for `CliErrorKind` code/exit-code mapping.
- `T-02`: `shared/output.rs` tests for typed classification across all kinds.
- `T-03`: regression test proving two different messages with the same typed
  kind map to the same exit code.
- `T-04`: `emit_cli_error` JSON envelope assertions remain unchanged.
- `T-05`: non-JSON invalid-input hint assertion for typed invalid-input errors.
- `T-06`: representative command-path tests for typed not-found/conflict and
  unavailable surfaces.

## Verification Matrix

| Requirement area | Verification method |
| --- | --- |
| FR-01, FR-02 | unit tests for typed error kind/value |
| FR-03 | compile checks + constructor usage in migrated call sites |
| FR-04 | `output.rs` classifier tests; code inspection for removed substring scanning |
| FR-05 | targeted tests in parsing and selected operation/runtime modules |
| FR-06 | JSON/non-JSON error emission tests |
| FR-07 | message mutation invariant tests |
| FR-08 | targeted crate tests for `orchestrator-cli` |

## Implementation Notes Input (Next Phase)
Primary implementation surfaces:
- `crates/orchestrator-cli/src/shared/output.rs`
- `crates/orchestrator-cli/src/shared.rs`
- `crates/orchestrator-cli/src/shared/parsing.rs`
- representative files under:
  - `crates/orchestrator-cli/src/services/operations/`
  - `crates/orchestrator-cli/src/services/runtime/`
  - `crates/orchestrator-cli/src/shared/runner.rs`
- `crates/orchestrator-cli/src/main.rs` (behavior verification only; likely no
  logic change required)

## Deterministic Deliverables for Implementation Phase
- Typed CLI error hierarchy and constructors.
- Exit-code classifier driven by typed metadata only.
- Migrated high-value call sites for invalid/not-found/conflict/unavailable.
- Regression tests locking typed classification behavior and envelope
  compatibility.
