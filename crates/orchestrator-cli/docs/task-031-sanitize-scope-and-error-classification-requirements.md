# TASK-031 Requirements: Consolidate Repository Scope Helpers and Error Classification

## Phase
- Workflow phase: `requirements`
- Workflow ID: `04065173-1028-47cf-bd94-c9a1894b913e`
- Task: `TASK-031`

## Objective
Define a deterministic, single-source implementation contract for:
- repository scope helper logic (`sanitize_identifier` +
  `repository_scope_for_path`)
- CLI/web error classification (`classify_error`)

Goal: remove cross-crate drift while preserving current operator-facing
contracts and exit-code semantics.

## Current Baseline Audit

| Surface | Current location | Current behavior | Gap |
| --- | --- | --- | --- |
| Repository scope helper variant A | `crates/orchestrator-core/src/services.rs` | hash input uses canonical path string, slug source uses raw `path.file_name()` | local duplicate + slug source differs from other call sites |
| Repository scope helper variant B | `crates/orchestrator-core/src/services/runner_helpers.rs` | hash input uses canonical path string, slug source uses canonical basename | local duplicate |
| Repository scope helper variant B | `crates/orchestrator-cli/src/shared/runner.rs` | hash input uses canonical path string, slug source uses canonical basename | local duplicate |
| Repository scope helper in tests | `crates/orchestrator-core/src/services/tests.rs` | copied algorithm (variant A) used for expected paths | test logic can drift from production |
| CLI error classifier | `crates/orchestrator-cli/src/shared/output.rs` | superset pattern coverage including clap/help-style and OS not-found strings | diverges from web classifier |
| Web error classifier | `crates/orchestrator-web-contracts/src/utils/classify_error.rs` | smaller pattern set | same message can map to different code/exit_code |

## Scope
In scope for implementation after this requirements phase:
- add one canonical repository scope helper module in `crates/protocol`
- make `orchestrator-core` and `orchestrator-cli` repository-scope call sites
  use the canonical helper
- remove copied repository-scope algorithm from
  `crates/orchestrator-core/src/services/tests.rs`
- add one canonical error-message classifier module in `crates/protocol`
- make CLI and `orchestrator-web-contracts` classify through the canonical
  classifier while preserving existing public function signatures at call sites
- add targeted parity and regression tests for helper behavior and exit-code
  mapping

Out of scope:
- changing CLI envelope schema (`ao.cli.v1`)
- changing exit-code mapping values (`2/3/4/5/1`)
- changing hash algorithm or suffix length (`12` lowercase hex chars)
- deduplicating unrelated identifier sanitizers (for example
  `sanitize_identifier_for_git`, `sanitize_identifier_for_path`)
- changing daemon scheduling/workflow/task-control behavior
- manual edits to `/.ao/*.json`

## Constraints
- Keep behavior deterministic and repository-safe:
  - scope format remains `<sanitized_repo_name>-<12 hex chars>`
  - fallback slug remains `"repo"` for empty/invalid identifiers
  - hash input remains canonicalized path string bytes
- Resolve slug-source ambiguity by standardizing repo slug derivation on the
  canonicalized path basename.
- Keep dependency graph acyclic:
  - canonical helpers must be in a crate consumed by
    `orchestrator-core`, `orchestrator-cli`, and
    `orchestrator-web-contracts` without reverse dependency edges.
- Preserve case-insensitive message matching and CLI help-hint behavior.
- Preserve classifier precedence order:
  `invalid_input -> not_found -> conflict -> unavailable -> internal`.

## Functional Requirements

### FR-01: Canonical Repository Scope API
- `crates/protocol` must export one canonical API for repository scope logic:
  - `sanitize_identifier(&str) -> String`
  - `repository_scope_for_path(&Path) -> String`
- Production copies of these algorithms in:
  - `crates/orchestrator-core/src/services.rs`
  - `crates/orchestrator-core/src/services/runner_helpers.rs`
  - `crates/orchestrator-cli/src/shared/runner.rs`
  must be removed.

### FR-02: Repository Scope Output Stability
- Scope output remains `<slug>-<12hex>` with existing normalization rules:
  - keep alphanumeric characters (lowercased)
  - map spaces/`_`/`-` to `-`
  - collapse repeated `-`
  - trim leading/trailing `-`
  - fallback `"repo"` when empty
- Hash derivation remains first six bytes of SHA-256 over canonical path string
  bytes.
- Slug source is explicitly standardized on canonical basename.

### FR-03: Test Drift Elimination
- `crates/orchestrator-core/src/services/tests.rs` must not contain a copied
  repository-scope algorithm.
- Tests must assert expected behavior through shared helper APIs (or wrappers
  that delegate directly to them).

### FR-04: Canonical Error Classification API
- `crates/protocol` must export one canonical message classifier:
  - `classify_error_message(&str) -> (&'static str, i32)`
- Production message-matching logic must no longer be duplicated in:
  - `crates/orchestrator-cli/src/shared/output.rs`
  - `crates/orchestrator-web-contracts/src/utils/classify_error.rs`

### FR-05: Classifier Pattern Coverage Parity
- Shared classifier must include CLI-equivalent pattern coverage:
  - invalid input: `invalid`, `parse`, `missing required`,
    `required arguments were not provided`, `unexpected argument`,
    `unknown argument`, `unrecognized option`, `confirmation_required`,
    `must be`
  - not found: `not found`, `no such file or directory`, `does not exist`
  - conflict: `already`, `conflict`
  - unavailable: `timed out`, `timeout`, `connection`, `unavailable`,
    `failed to connect`

### FR-06: Call-Site API Compatibility
- CLI wrapper signature remains:
  `classify_error(&anyhow::Error) -> (&'static str, i32)`.
- Web-contracts signature remains:
  `classify_error(&str) -> (&'static str, i32)`.
- `orchestrator-web-api` continues to consume
  `orchestrator_web_contracts::classify_error` without envelope shape changes.

### FR-07: Regression Coverage
- Add/adjust tests validating:
  - canonical-basename scope behavior
  - repository scope parity across core/cli call paths
  - classifier parity across CLI and web-contracts adapters
  - unchanged exit-code mapping and CLI help-hint behavior

## Acceptance Criteria
- `AC-01`: one canonical production implementation exists for repository scope
  helper logic.
- `AC-02`: the three production repository scope call sites consume canonical
  helper APIs; local copies are removed.
- `AC-03`: `services/tests.rs` no longer contains copied repository-scope
  algorithm code.
- `AC-04`: one canonical message classifier implementation exists and is used by
  CLI and web-contracts.
- `AC-05`: web-contracts classification coverage matches CLI superset patterns
  listed in FR-05.
- `AC-06`: CLI envelope and exit-code mappings remain unchanged.
- `AC-07`: canonical-basename slug derivation is explicit and tested.
- `AC-08`: changes remain scoped to helper consolidation and tests without
  unrelated behavior drift.

## Testable Acceptance Checklist
- `T-01`: protocol unit tests for repository scope normalization and suffix
  generation.
- `T-02`: core/cli tests proving repository-scope call paths use shared helper
  behavior.
- `T-03`: protocol classifier unit tests for invalid/not-found/conflict/
  unavailable/internal precedence.
- `T-04`: CLI tests validating wrapper mapping and help-hint behavior remain
  intact.
- `T-05`: web-contracts/web-api tests validating parity for under-classified
  messages (`required arguments`, `unknown argument`, `no such file or
  directory`, `does not exist`, `timeout`).

## Verification Matrix
| Requirement area | Verification method |
| --- | --- |
| FR-01, FR-02, FR-03 | protocol helper tests + compile coverage from refactored call sites |
| FR-04, FR-05 | protocol classifier tests + CLI/web-contracts parity assertions |
| FR-06 | CLI/output regression tests + web-api conversion checks |
| FR-07 | targeted tests across protocol/core/cli/web-contracts/web-api |

## Implementation Notes (Input to Next Phase)
Primary change targets:
- `crates/protocol/src/lib.rs` (exports)
- `crates/protocol/src/repository_scope.rs` (new)
- `crates/protocol/src/error_classification.rs` (new)
- `crates/orchestrator-core/src/services.rs`
- `crates/orchestrator-core/src/services/runner_helpers.rs`
- `crates/orchestrator-core/src/services/tests.rs`
- `crates/orchestrator-cli/src/shared/runner.rs`
- `crates/orchestrator-cli/src/shared/output.rs`
- `crates/orchestrator-web-contracts/src/utils/classify_error.rs`
- `crates/orchestrator-web-contracts/Cargo.toml` (add `protocol` dependency)
- `crates/orchestrator-web-api/src/models/web_api_error.rs` (only if adapter
  changes are required)

## Deterministic Deliverables for Implementation Phase
- one shared repository scope helper implementation consumed by the targeted
  production surfaces
- one shared error classifier consumed by CLI and web-contracts
- updated tests proving deterministic parity and unchanged exit-code contract
