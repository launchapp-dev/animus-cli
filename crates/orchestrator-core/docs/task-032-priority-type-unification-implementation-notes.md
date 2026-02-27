# TASK-032 Implementation Notes: Priority Type Unification Across Protocol and Core

## Purpose
Convert the requirements decision into a low-risk implementation plan that
disambiguates priority type naming without changing runtime behavior or wire
format.

## Chosen Strategy
Disambiguate protocol naming rather than merging core/protocol domain types:
- Protocol MoSCoW enum becomes `RequirementPriority` (canonical).
- Preserve `protocol::Priority` as compatibility alias during migration.
- Keep core task priority enum name unchanged (`Priority`).
- Keep core requirement priority enum name unchanged (`RequirementPriority`).

This removes the main naming ambiguity while avoiding dependency/layer churn.

## Non-Negotiable Constraints
- Keep serialized requirement priority values exactly:
  `must|should|could|wont`.
- Do not change task priority semantics or accepted values.
- Keep changes scoped to priority type definitions and direct call sites.
- No direct/manual edits to `/.ao/*.json`.

## Proposed Change Surface

### Protocol Type Canonicalization
- `crates/protocol/src/common.rs`
  - rename canonical enum symbol from `Priority` to `RequirementPriority`.
  - keep `Priority` as compatibility alias (optionally deprecated) to minimize
    external break risk.
  - preserve serde shape (`#[serde(rename_all = "lowercase")]`).

### Protocol Daemon Model Clarification
- `crates/protocol/src/daemon.rs`
  - update `RequirementNode.priority` to `RequirementPriority`.

### Core Type Documentation Clarification
- `crates/orchestrator-core/src/types.rs`
  - add short doc comments clarifying:
    - `Priority` => task urgency
    - `RequirementPriority` => requirement MoSCoW priority
  - no enum variant/value changes.

### Test Updates
- `crates/protocol/tests/daemon.rs`
  - add/adjust tests constructing requirement-like payloads with
    `RequirementPriority`.
- `crates/protocol/tests/compat_serialization.rs`
  - assert canonical type serialization remains unchanged.
  - optional compatibility assertion using alias (with `#[allow(deprecated)]`
    if deprecation is enabled).

## Compatibility Contract
- Wire compatibility: unchanged, because enum variant strings do not change.
- Rust API compatibility: retained via alias path for existing `Priority`
  imports.
- Protocol version: no bump required for symbol-only disambiguation with stable
  payload shape.

## Implementation Sequence
1. Update protocol common enum naming and compatibility alias.
2. Update protocol daemon model to use canonical name.
3. Add/adjust protocol tests for canonical naming and unchanged serialization.
4. Add core enum doc comments for role clarity.
5. Run targeted protocol and core tests.

## Risks and Mitigations
- Risk: external code relies on `protocol::Priority` concrete enum name.
  - Mitigation: maintain compatibility alias in this change.
- Risk: accidental serialization drift from enum changes.
  - Mitigation: explicit serialization tests in `protocol/tests`.
- Risk: over-scoped refactor from searching `Priority` globally.
  - Mitigation: limit edits to protocol MoSCoW definitions and directly related
    call sites.

## Validation Targets
- `cargo test -p protocol`
- `cargo test -p orchestrator-core services::tests`
