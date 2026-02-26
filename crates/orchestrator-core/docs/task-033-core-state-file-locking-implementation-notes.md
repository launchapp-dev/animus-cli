# TASK-033 Implementation Notes: Core State File Locking

## Purpose
Translate TASK-033 requirements into a concrete implementation plan that
eliminates cross-process read-modify-write races for `.ao/core-state.json`.

## Non-Negotiable Constraints
- Keep changes scoped to `crates/orchestrator-core`.
- Preserve `.ao/core-state.json` schema and existing service contracts.
- Avoid direct/manual edits to `/.ao/*.json`.
- Keep locking and tests deterministic and cross-platform.

## Proposed Change Surface

### Dependency Placement
- `crates/orchestrator-core/Cargo.toml`
  - add `fs2 = "0.4"` so locking resides in the crate that performs
    persistence.

### Shared Lock + Transaction Helper
- `crates/orchestrator-core/src/services.rs`
  - add internal lock file resolver for core state persistence
    (for example `<ao-dir>/core-state.lock`).
  - add helper to open/create lock file and acquire exclusive lock.
  - add a single transaction entrypoint for file-backed mutations:
    - lock file (exclusive),
    - reload latest `CoreState` from disk,
    - apply mutation closure,
    - persist `core-state.json`,
    - persist structured artifacts,
    - update in-memory state,
    - release lock via RAII.

### File-Backed Mutator Refactor
- `crates/orchestrator-core/src/services/task_impl.rs`
- `crates/orchestrator-core/src/services/planning_impl.rs`
- `crates/orchestrator-core/src/services/project_impl.rs`
- `crates/orchestrator-core/src/services/workflow_impl.rs`
- `crates/orchestrator-core/src/services/daemon_impl.rs`
  - replace "mutate local memory then call `persist_snapshot`" paths with the
    shared transaction helper so ID generation and state mutation occur on the
    current locked snapshot.

### Test Coverage Additions
- `crates/orchestrator-core/src/services/tests.rs`
  - add concurrent hub regression tests:
    - two `FileServiceHub` instances create requirements concurrently,
    - two `FileServiceHub` instances create tasks concurrently,
    - daemon/status mutation interleaves with another mutator safely.
  - assert:
    - no duplicate IDs,
    - both updates retained,
    - resulting `core-state.json` parses,
    - structured artifacts remain coherent.

## Locking Strategy

### Lock Scope
- Lock surrounds the full read-modify-write transaction, not just file write.
- Lock is acquired before loading disk state and held until commit completes.

### Lock Ordering
- Define a single ordering to avoid deadlocks:
  1. file lock (cross-process),
  2. in-process `RwLock` write guard,
  3. mutation + persist,
  4. release `RwLock`,
  5. release file lock.

### Failure Behavior
- If lock acquisition or persistence fails, return contextual error and leave
  existing committed state unchanged.
- No best-effort partial commit behavior.

## Implementation Sequence
1. Add `fs2` to `orchestrator-core` dependencies.
2. Implement lock/transaction helper in `services.rs`.
3. Refactor one mutator family (`requirements`/`tasks`) to validate approach.
4. Refactor remaining mutator families (`project`, `workflow`, `daemon`).
5. Add/expand concurrent regression tests.
6. Run targeted tests, then full `orchestrator-core` test pass.

## Risks and Mitigations
- Risk: blocking lock calls inside async methods increase latency.
  - Mitigation: keep critical section minimal and avoid unnecessary I/O while
    lock is held.
- Risk: deadlock from inconsistent lock ordering.
  - Mitigation: enforce one ordering in shared helper; avoid ad hoc locking.
- Risk: subtle behavior regression from refactor breadth.
  - Mitigation: keep mutation logic centralized and preserve existing API-level
    tests while adding new race regressions.

## Validation Targets
- `cargo test -p orchestrator-core services::tests`
- `cargo test -p orchestrator-core`
