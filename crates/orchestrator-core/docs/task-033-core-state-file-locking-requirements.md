# TASK-033 Requirements: Core State File Locking

## Phase
- Workflow phase: `requirements`
- Workflow ID: `e8480b12-f900-4606-870a-6332102b0e6f`
- Task: `TASK-033`

## Objective
Define a deterministic, cross-process persistence contract for
`.ao/core-state.json` so concurrent daemon and CLI mutations do not cause
lost updates, duplicate IDs, or on-disk state corruption.

## Current Baseline Audit

| Surface | Current location | Current state | Gap |
| --- | --- | --- | --- |
| Atomic JSON write | `crates/orchestrator-core/src/services.rs` (`write_json_atomic`) | temp file + rename replacement | avoids partial-file writes, but no cross-process coordination |
| Snapshot persistence | `crates/orchestrator-core/src/services.rs` (`persist_snapshot`) | writes snapshot + structured artifacts | no lock across read-modify-write lifecycle |
| State loading | `crates/orchestrator-core/src/services/state_store.rs` (`load_core_state`) | read at hub startup | long-lived in-memory state can become stale vs concurrent writers |
| Mutating APIs | `task_impl.rs`, `planning_impl.rs`, `project_impl.rs`, `workflow_impl.rs`, `daemon_impl.rs` | mutate in-memory lock then persist | two processes can generate IDs from stale snapshots (REQ-022/023 collision class) |

## Problem Statement
Current persistence is process-local safe but not cross-process safe. Two
`FileServiceHub` instances can:
- read the same logical state,
- independently derive new IDs (for example, both picking `REQ-023`),
- persist sequentially with last-writer-wins behavior.

Result: collisions, lost updates, and inconsistent structured artifacts.

## Scope
In scope for implementation after this requirements phase:
- Add process-shared file locking (for example via `fs2::FileExt` or equivalent)
  to guard `.ao/core-state.json` mutations.
- Move file-backed mutations to a coordinated read-modify-write transaction:
  - acquire exclusive lock,
  - reload current on-disk `CoreState`,
  - apply mutation,
  - persist `core-state.json` and structured artifacts,
  - update in-memory state.
- Keep read paths deterministic when writers are active.
- Add regression tests that prove no ID collisions or lost updates with
  concurrent writers.

Out of scope:
- Schema changes to `.ao/core-state.json`.
- CLI envelope / exit code changes.
- Manual editing of `/.ao/*.json`.
- Changes to workflow/task semantics unrelated to persistence coordination.

## Constraints
- Preserve current on-disk JSON schema and backward compatibility.
- Keep locking cross-platform (Unix + non-Unix behavior explicit and tested).
- Ensure lock release is automatic and robust on error paths.
- Avoid deadlock between async state locks and file locks by defining one lock
  acquisition order.
- Keep behavior deterministic under test (bounded waits, no unbounded retries).

## Functional Requirements

### FR-01: Cross-Process Mutual Exclusion
- Every file-backed state mutation must execute under an exclusive process-shared
  lock.
- Lock scope must include the full read-modify-write sequence, not only final
  file replace.

### FR-02: Stale Snapshot Elimination for Mutations
- Under lock, mutation paths must operate on the latest on-disk state before
  deriving IDs or applying updates.
- In-memory `FileServiceHub` state must be refreshed from the committed
  snapshot.

### FR-03: Persistence Integrity
- `core-state.json` and structured artifacts remain synchronized for each
  committed mutation.
- Failures during persistence do not leave partial commit state.

### FR-04: Concurrency Regression Coverage
- Concurrency tests must cover at least:
  - concurrent requirement creation across two `FileServiceHub` instances,
  - concurrent task creation across two `FileServiceHub` instances,
  - daemon status/log mutation interleaving with other mutations.
- Tests must prove unique ID allocation and retention of all committed records.

### FR-05: Operational Safety
- Lock contention handling must be bounded and observable in error context.
- No new dependency on desktop-wrapper frameworks; Rust-only workspace policy
  remains unchanged.

## Acceptance Criteria
- `AC-01`: All file-backed mutation paths use a shared locking/transaction helper
  that enforces exclusive cross-process coordination.
- `AC-02`: Concurrent writers do not produce duplicate requirement or task IDs.
- `AC-03`: Concurrent writes retain both writers' logical updates (no
  last-writer data loss for covered scenarios).
- `AC-04`: `core-state.json` remains valid JSON and structured artifact outputs
  stay consistent after concurrent mutation tests.
- `AC-05`: Locking logic is cross-platform and covered by deterministic tests in
  `orchestrator-core`.
- `AC-06`: Existing `orchestrator-core` behavior outside persistence
  coordination remains unchanged.

## Testable Acceptance Checklist
- `T-01`: Unit tests for lock helper acquire/release behavior and error paths.
- `T-02`: Integration test with two hubs concurrently creating requirements and
  asserting unique IDs + both records present.
- `T-03`: Integration test with two hubs concurrently creating tasks and
  asserting unique IDs + both records present.
- `T-04`: Regression test for daemon + non-daemon mutation interleaving with no
  state corruption.
- `T-05`: Full crate test pass for `orchestrator-core`.

## Verification Matrix
| Requirement area | Verification method |
| --- | --- |
| Cross-process locking | lock-helper unit tests + concurrent hub integration tests |
| ID collision prevention | parallel create tests asserting unique/monotonic IDs |
| Persistence integrity | JSON parse + structured artifact assertions after race tests |
| Backward compatibility | existing service behavior tests remain green |

## Implementation Notes (Input to Next Phase)
Primary change targets:
- `crates/orchestrator-core/Cargo.toml`
  - add `fs2` dependency in this crate (locking is implemented where
    persistence occurs).
- `crates/orchestrator-core/src/services.rs`
  - introduce a file-lock-backed mutation transaction helper used by all
    file-backed mutators.
- `crates/orchestrator-core/src/services/{task_impl.rs,planning_impl.rs,project_impl.rs,workflow_impl.rs,daemon_impl.rs}`
  - route persistence through the shared transaction path.
- `crates/orchestrator-core/src/services/tests.rs` (and/or targeted module tests)
  - add concurrent writer regression coverage for REQ/TASK ID generation.

## Deterministic Deliverables for Implementation Phase
- Shared, exclusive, file-lock-coordinated mutation flow for `core-state.json`.
- Refactored file-backed mutating APIs using that flow.
- Concurrency regression tests proving collision/lost-update fixes.
- No schema drift and no manual `.ao` file edits.
