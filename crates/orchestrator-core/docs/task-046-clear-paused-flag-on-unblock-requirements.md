# TASK-046 Requirements: Clear `paused` When Leaving Blocked/OnHold

## Phase
- Workflow phase: `requirements`
- Workflow ID: `063d32c7-fbae-4038-a591-f840ee65835e`
- Task: `TASK-046`

## Objective
Guarantee task status consistency by clearing the `paused` flag whenever a task
transitions from blocked states (`blocked`, `on-hold`) to a non-blocked state.
This prevents ghost states such as `status=ready` with `paused=true`.

## Current Baseline Audit

| Surface | Current location | Current behavior | Gap |
| --- | --- | --- | --- |
| Status transition mutation | `crates/orchestrator-core/src/services/task_shared.rs` (`apply_task_status`) | status, cancelled, timestamps, and blocked metadata are updated in one function | unblock correctness should be explicitly guaranteed in the unblock branch |
| Blocked metadata cleanup | `crates/orchestrator-core/src/services/task_shared.rs` (`if !status.is_blocked()`) | clears `blocked_reason`, `blocked_at`, `blocked_phase`, `blocked_by` | branch does not explicitly write `paused = false`, making unblock invariant less explicit |
| Blocked-state model | `crates/orchestrator-core/src/types.rs` (`TaskStatus::is_blocked`) | `blocked` and `on-hold` are both treated as blocked states | unblock behavior must be verified for both blocked variants |
| Runtime call paths | `crates/orchestrator-core/src/services/task_impl.rs` (`set_status`, `update`) | status changes route through `apply_task_status` | status invariant failures in shared helper affect all task updates |
| Regression coverage | `crates/orchestrator-core/src/services/tests.rs` | tests cover priority/dependency flows, but not paused reset transitions | paused ghost-state regression is not explicitly guarded |

## Problem Statement
`apply_task_status` must enforce that blocked-state cleanup and pause-state
cleanup happen together. Without an explicit unblock guarantee for `paused`,
task records can drift into inconsistent status combinations.

## Decision for Implementation
- Keep unblock behavior centralized in `apply_task_status`.
- On any transition to a non-blocked status, explicitly set `paused = false`
  together with blocked metadata cleanup.
- Add regression tests covering both `blocked -> ready` and
  `on-hold -> in-progress` transitions.
- Keep scope narrow: no changes to task status taxonomy or scheduling policy.

## Scope
In scope for implementation after this requirements phase:
- Update task status mutation logic so non-blocked statuses clear `paused`.
- Preserve existing blocked metadata cleanup fields.
- Add regression tests for pause/blocked consistency across transitions.

Out of scope:
- Task lifecycle redesign (`TaskStatus` enum semantics).
- Daemon pause semantics (`DaemonStatus::Paused`) or runner behavior.
- Direct/manual edits to `/.ao/*.json`.

## Constraints
- Keep behavior deterministic for both in-memory and file-backed hubs.
- Keep mutation centralized in existing shared task helper.
- Preserve current timestamp behavior (`started_at`, `completed_at`) and
  cancellation behavior.
- Avoid broad refactors unrelated to blocked/unblocked state consistency.

## Functional Requirements

### FR-01: Blocked States Must Pause Tasks
- When status is `blocked` or `on-hold`, task `paused` must be `true`.

### FR-02: Non-Blocked States Must Unpause Tasks
- When status transitions to any non-blocked state (`backlog`, `ready`,
  `in-progress`, `done`, `cancelled`), task `paused` must be `false`.

### FR-03: Blocked Metadata Cleanup Must Stay Coupled to Unblock
- On non-blocked status, `blocked_reason`, `blocked_at`, `blocked_phase`, and
  `blocked_by` must all be cleared.

### FR-04: Existing Status Side Effects Must Be Preserved
- `started_at` behavior for first `in-progress` transition remains unchanged.
- `completed_at` behavior for first `done` transition remains unchanged.
- `cancelled` flag semantics remain unchanged.

### FR-05: Regression Coverage for Ghost-State Prevention
- Tests must assert there is no persisted state where `status` is non-blocked
  and `paused` remains true after applying a status transition.

## Acceptance Criteria
- `AC-01`: After `blocked -> ready`, `paused == false`.
- `AC-02`: After `on-hold -> in-progress`, `paused == false`.
- `AC-03`: After transition to any non-blocked state, blocked metadata fields
  are all `None`.
- `AC-04`: While status is `blocked` or `on-hold`, `paused == true`.
- `AC-05`: Existing started/completed timestamp behavior remains unchanged.

## Testable Acceptance Checklist
- `T-01`: Add unit/integration regression in
  `crates/orchestrator-core/src/services/tests.rs` validating
  `blocked -> ready` clears `paused` and blocked fields.
- `T-02`: Add regression validating `on-hold -> in-progress` clears `paused`
  and blocked fields.
- `T-03`: Run targeted test suite:
  - `cargo test -p orchestrator-core services::tests -- --nocapture`

## Verification Matrix

| Requirement area | Verification method |
| --- | --- |
| FR-01, FR-02 | status transition tests asserting `paused` values per state |
| FR-03 | transition tests asserting all blocked metadata fields are cleared on unblock |
| FR-04 | existing status-side-effect tests (or added assertions) for timestamp/cancelled stability |
| FR-05 | targeted `services::tests` run covering new ghost-state regressions |

## Implementation Notes Input (Next Phase)
Primary implementation surfaces:
- `crates/orchestrator-core/src/services/task_shared.rs`
  - enforce explicit `paused = false` in non-blocked cleanup path.
- `crates/orchestrator-core/src/services/tests.rs`
  - add status transition regression tests for blocked/on-hold unpause behavior.

## Deterministic Deliverables for Implementation Phase
- Non-blocked task statuses cannot retain `paused=true`.
- Blocked metadata and pause state are cleared together on unblock.
- Regression coverage protects against reintroduction of ghost paused states.
