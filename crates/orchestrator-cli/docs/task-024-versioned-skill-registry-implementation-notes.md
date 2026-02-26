# TASK-024 Implementation Notes: Versioned Skill Registry Lifecycle

## Purpose
Translate TASK-024 requirements into a concrete implementation plan for
`orchestrator-cli` that introduces skill lifecycle commands with deterministic
resolution and reproducible lockfile behavior.

## Non-Negotiable Constraints
- Keep implementation in Rust under `crates/`.
- Keep `.ao` mutations command-driven; do not manually edit `.ao` state files.
- Preserve `ao.cli.v1` output envelope behavior and current exit-code mapping.
- Keep command behavior deterministic and repository-safe.

## Proposed Change Surface

### CLI and dispatch wiring
- `crates/orchestrator-cli/src/cli_types.rs`
  - add `Command::Skill { command: SkillCommand }`
  - add `SkillCommand` enum:
    - `Search`
    - `Install`
    - `List`
    - `Update`
    - `Publish`
  - add argument structs for each subcommand.
- `crates/orchestrator-cli/src/main.rs`
  - route `Command::Skill` to new operations handler.
- `crates/orchestrator-cli/src/services/operations.rs`
  - register/export new `ops_skill` module.

### Skill operations module
- New module tree:
  - `crates/orchestrator-cli/src/services/operations/ops_skill.rs`
  - `crates/orchestrator-cli/src/services/operations/ops_skill/model.rs`
  - `crates/orchestrator-cli/src/services/operations/ops_skill/store.rs`
  - `crates/orchestrator-cli/src/services/operations/ops_skill/resolver.rs`

Module responsibilities:
- `ops_skill.rs`
  - command handler entrypoint and per-subcommand orchestration.
- `model.rs`
  - serde models for:
    - `SkillRegistryStateV1`
    - `SkillLockStateV1`
    - `SkillVersionRecord`
    - `ResolvedSkillEntry`
- `store.rs`
  - path helpers for:
    - `.ao/state/skills-registry.v1.json`
    - `.ao/state/skills-lock.v1.json`
  - load/save using `read_json_or_default` and `write_json_pretty`.
- `resolver.rs`
  - precedence-aware version resolution logic with deterministic tie-breakers.

## Resolver Plan
Deterministic selection algorithm:
1. Build candidate set from selected registry source(s).
2. Apply explicit CLI constraints first.
3. Apply lock pin/default constraints if present.
4. Apply project registry constraints.
5. Select winner by:
   - stable over pre-release unless explicitly allowed
   - highest semver
   - lexical source tie-break

The resolver should return:
- selected record
- normalized resolution metadata for lockfile persistence
- optional reasoning trace for future debugging/tests

## Lockfile Plan
Lockfile behavior requirements to encode in implementation:
- entries keyed by `name` + `source`
- stable ordering (`name`, then `source`)
- no non-deterministic fields
- write file only on logical changes

Install/update behavior:
- `skill install` inserts or replaces a single lock entry.
- `skill update` rewrites only entries whose resolved target changed.
- repeated no-op install/update leaves lockfile bytes unchanged.

## Publish Plan
`skill publish` flow:
1. Validate required metadata (`name`, `version`, source).
2. Validate semver parseability.
3. Compute/persist integrity metadata.
4. Reject duplicate version for same skill/source with `conflict`.
5. Persist catalog update in deterministic order.

## Testing Plan

### New CLI lifecycle test module
- `crates/orchestrator-cli/tests/cli_skill_lifecycle.rs`

Coverage goals:
- subcommand parse/dispatch coverage
- deterministic `search` ordering
- `install` writes registry + lock entries
- `list` reflects persisted state
- `update` respects precedence and mutation boundaries
- `publish` duplicate conflict path
- lockfile byte-stability on repeated no-op runs
- representative JSON success/error envelope checks

### Unit-level resolver tests
- `crates/orchestrator-cli/src/services/operations/ops_skill/resolver.rs`
  - precedence ordering cases
  - prerelease filtering behavior
  - tie-break determinism

## Implementation Sequence
1. Add CLI command/arg definitions for `skill` lifecycle.
2. Wire command dispatch in `main.rs` and `operations.rs`.
3. Implement state models + store layer for v1 skill files.
4. Implement deterministic resolver logic.
5. Implement `search/install/list/update/publish` handlers.
6. Add CLI and unit tests.
7. Run targeted test commands and fix introduced failures.

## Validation Targets (Implementation Phase)
- `cargo test -p orchestrator-cli --test cli_skill_lifecycle`
- `cargo test -p orchestrator-cli --test cli_json_contract`
- `cargo test -p orchestrator-cli --test cli_smoke --test cli_e2e`

## Risks and Mitigations
- Risk: ambiguity in resolver precedence interpretation.
  - Mitigation: encode precedence table in unit tests and requirements doc.
- Risk: lockfile churn from unstable ordering.
  - Mitigation: sort entries before write and assert byte-stability.
- Risk: command scope drift into non-skill domains.
  - Mitigation: keep module boundaries isolated to `ops_skill` and dispatch
    wiring only.
