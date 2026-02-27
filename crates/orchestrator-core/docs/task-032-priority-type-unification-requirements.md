# TASK-032 Requirements: Priority Type Unification Across Protocol and Core

## Phase
- Workflow phase: `requirements`
- Workflow ID: `39bc1d46-aeb4-4bdf-99cf-4f723bf36a42`
- Task: `TASK-032`

## Objective
Eliminate ambiguous "Priority" naming across crates while preserving behavior
and wire compatibility. The implementation must make it clear which priority
system is for requirements (MoSCoW) and which is for tasks (Critical/High/Medium/Low).

## Current Baseline Audit

| Surface | Current location | Current state | Gap |
| --- | --- | --- | --- |
| Protocol MoSCoW priority | `crates/protocol/src/common.rs` | `enum Priority { Must, Should, Could, Wont }` serialized lowercase | generic name collides semantically with task priority naming |
| Protocol daemon requirement model | `crates/protocol/src/daemon.rs` | `RequirementNode.priority: Priority` | unclear that this field is requirement-level MoSCoW priority |
| Core task priority | `crates/orchestrator-core/src/types.rs` | `enum Priority { Critical, High, Medium, Low }` | same enum name with different semantics from protocol MoSCoW priority |
| Core requirement priority | `crates/orchestrator-core/src/types.rs` | `enum RequirementPriority { Must, Should, Could, Wont }` | duplicated concept in protocol with a different type name |

## Problem Statement
There are two distinct priority concepts:
- Requirement prioritization (MoSCoW: `must|should|could|wont`)
- Task urgency ordering (`critical|high|medium|low`)

Today both concepts use the name `Priority` in different crates, which creates
confusion during imports, code review, and refactors. The protocol MoSCoW enum
is also named too generically despite being requirement-specific.

## Decision for Implementation
Use **disambiguation with compatibility**, not a cross-crate type merge:
- Protocol canonical MoSCoW enum name becomes `RequirementPriority`.
- `protocol::Priority` remains as a compatibility alias during transition.
- Core keeps:
  - `Priority` for task urgency
  - `RequirementPriority` for requirement MoSCoW

This keeps layering stable and avoids introducing new inter-crate coupling.

## Scope
In scope for implementation after this requirements phase:
- Rename protocol MoSCoW enum symbol to `RequirementPriority`.
- Update protocol daemon models to use `RequirementPriority` explicitly.
- Add compatibility alias (`Priority`) in protocol for non-breaking migration.
- Add clarifying documentation/comments in core type definitions where needed.
- Add/adjust tests to prove unchanged wire serialization and compatibility.

Out of scope:
- Changing task priority semantics (`critical|high|medium|low`).
- Changing requirement priority semantics (`must|should|could|wont`).
- Protocol version bump when payload shape is unchanged.
- Any `.ao` state schema changes.

## Constraints
- Preserve serialized JSON values for requirement priorities (`must`, `should`,
  `could`, `wont`).
- Preserve `RequirementNode` wire shape except symbol-level Rust naming.
- Keep changes deterministic and limited to protocol/core priority typing.
- Avoid broad refactors unrelated to priority-type disambiguation.

## Functional Requirements

### FR-01: Canonical Requirement Priority Naming in Protocol
- Protocol must expose MoSCoW priority under `RequirementPriority`.
- `RequirementNode.priority` must use `RequirementPriority`.

### FR-02: Backward-Compatible Rust API Transition
- Protocol must provide a compatibility path for existing `Priority` imports
  (type alias allowed).
- Compatibility mechanism must not change wire serialization.

### FR-03: Core Priority Role Clarity
- Core must keep task priority and requirement priority as separate concepts.
- Type-level documentation must explicitly describe each enum's intended domain.

### FR-04: No Behavioral Regression
- Task generation and requirement handling logic must behave identically to
  baseline mapping and parsing behavior.

### FR-05: Regression Coverage
- Tests must verify protocol serialization stability and that existing core
  priority behavior remains unchanged.

## Acceptance Criteria
- `AC-01`: Protocol has `RequirementPriority` as the canonical MoSCoW enum.
- `AC-02`: `protocol::Priority` remains available only as a compatibility path
  (for example, alias/deprecation), with no wire-format change.
- `AC-03`: `RequirementNode.priority` uses `RequirementPriority`.
- `AC-04`: JSON encoding/decoding for requirement priorities remains
  lowercase `must|should|could|wont`.
- `AC-05`: Core task priority (`critical|high|medium|low`) and requirement
  priority behavior are unchanged.
- `AC-06`: `cargo test -p protocol` and priority-relevant core tests pass.

## Testable Acceptance Checklist
- `T-01`: Protocol unit/serialization test for `RequirementPriority`
  roundtrip.
- `T-02`: Protocol compatibility test proving `Priority` compatibility path
  compiles and serializes identically.
- `T-03`: Existing core tests covering requirement/task priority behavior stay
  green.

## Verification Matrix
| Requirement area | Verification method |
| --- | --- |
| Protocol naming clarity | compile checks + type usage updates |
| Wire compatibility | protocol serialization tests |
| Core behavior stability | existing core tests + targeted priority checks |

## Implementation Notes (Input to Next Phase)
Primary expected change targets:
- `crates/protocol/src/common.rs`
  - introduce canonical `RequirementPriority` enum; keep compatibility alias.
- `crates/protocol/src/daemon.rs`
  - use `RequirementPriority` in `RequirementNode`.
- `crates/protocol/tests/{daemon.rs,compat_serialization.rs}`
  - add/adjust assertions for canonical naming + stable serialization.
- `crates/orchestrator-core/src/types.rs`
  - optional doc comments to harden semantic separation between task and
    requirement priority types.

## Deterministic Deliverables for Implementation Phase
- Canonical protocol MoSCoW type named `RequirementPriority`.
- Compatibility-preserving transition path for existing `Priority` imports.
- Explicitly disambiguated type usage in protocol daemon models.
- Test evidence that behavior and wire payloads are unchanged.
