# TASK-046 Implementation Notes: Pause-State Consistency on Unblock

## Purpose
Translate TASK-046 requirements into a minimal, low-risk implementation that
prevents ghost task states (`status` non-blocked while `paused=true`).

## Chosen Strategy
Implement a narrow invariant in the shared status mutator:
- On blocked statuses (`blocked`, `on-hold`), task remains paused.
- On non-blocked statuses, clear blocked metadata and explicitly clear pause
  state in the same branch.

Pair this with regression tests that cover the two key transition paths:
- `blocked -> ready`
- `on-hold -> in-progress`

## Non-Negotiable Constraints
- Keep changes scoped to `crates/orchestrator-core`.
- Preserve existing status side effects (`started_at`, `completed_at`,
  `cancelled`).
- Avoid direct/manual edits to `/.ao/*.json`.
- Keep behavior deterministic across in-memory and file-backed hubs.

## Proposed Change Surface

### 1) Shared Task Status Mutator
- File: `crates/orchestrator-core/src/services/task_shared.rs`
- Function: `apply_task_status`
- Change:
  - ensure non-blocked cleanup branch includes explicit `task.paused = false`
    with blocked metadata cleanup.
  - keep blocked branch behavior intact (default reason/time initialization).

### 2) Regression Tests
- File: `crates/orchestrator-core/src/services/tests.rs`
- Add tests that:
  - create task, transition `blocked -> ready`, assert:
    - `paused == false`,
    - `blocked_reason`, `blocked_at`, `blocked_phase`, `blocked_by` are `None`.
  - create task, transition `on-hold -> in-progress`, assert same unblock
    invariants.
  - optionally assert blocked statuses still force `paused == true`.

## Suggested Sequencing
1. Update `apply_task_status` unblock branch.
2. Add transition regression tests in `services/tests.rs`.
3. Run targeted core tests.
4. Fix regressions if introduced, without expanding task scope.

## Risks and Mitigations
- Risk: accidental behavior drift in existing timestamp/cancelled logic.
  - Mitigation: keep edits localized to pause/unblock branch and assert existing
    side effects in tests where practical.
- Risk: test coverage misses `on-hold` path.
  - Mitigation: include explicit `on-hold -> in-progress` transition test.
- Risk: broad refactor creeps into unrelated task lifecycle logic.
  - Mitigation: change only the shared helper and direct regression tests.

## Validation Targets
- `cargo test -p orchestrator-core services::tests -- --nocapture`
- Optional focused rerun if needed:
  - `cargo test -p orchestrator-core task -- --nocapture`
