# TASK-003 Implementation Notes: Runner Lifecycle Regression Tests

## Purpose
Translate TASK-003 requirements into concrete test work that hardens
`orchestrator-core` runner lifecycle behavior without changing public runtime
contracts.

## Non-Negotiable Constraints
- Keep changes scoped to `crates/orchestrator-core`.
- Preserve current daemon/runner behavior and external command contracts.
- Keep tests deterministic, isolated, and safe for shared CI runners.
- Avoid manual `.ao` state mutations.

## Proposed Change Surface

### Lifecycle test coverage
- `crates/orchestrator-core/src/services/runner_helpers.rs`
  - extend `#[cfg(test)]` coverage from compatibility checks to lifecycle
    branches:
    - stale lock cleanup with dead PID
    - stale unix socket cleanup (unix)
    - shutdown behavior for missing/dead/live lock paths

### Daemon fallback regression coverage
- `crates/orchestrator-core/src/services/daemon_impl.rs`
  - add targeted tests for startup fallback behavior:
    - first startup attempt fails, stop+retry succeeds
    - fallback retry failure preserves error context
  - keep tests focused on retry semantics, not unrelated daemon features

### Service-level lifecycle stability coverage
- `crates/orchestrator-core/src/services/tests.rs`
  - add scenario proving health-loss crash transition:
    - daemon active with known runner PID
    - readiness/liveness loss triggers `Crashed`
    - diagnostic log message is appended
  - add deterministic repeated start/stop lifecycle regression (20 cycles) with
    artifact checks after each cycle

### Optional seam extraction (only if needed for determinism)
- `crates/orchestrator-core/src/services/runner_helpers.rs`
  - extract narrow internal helpers for spawn/readiness/wait decision points so
    tests can validate lifecycle branches without requiring an externally built
    `agent-runner` binary
  - keep existing exported function signatures unchanged

## Deterministic Test Strategy
- Use `tempfile::TempDir` per test for project and runner config directories.
- Gate environment mutation with a local `EnvVarGuard` + global mutex pattern.
- Use fixture-owned processes only; capture PID and ensure teardown.
- On unix:
  - create/remove temporary socket files under the test config dir
  - avoid connections to global socket locations
- Keep wait loops short in tests (test seam or bounded timing) to prevent slow,
  flaky CI behavior.

## Implementation Sequence
1. Add/extend helper utilities for test env isolation (if missing).
2. Implement `runner_helpers` lifecycle branch tests (`RL-05`..`RL-08`).
3. Add daemon startup fallback tests (`RL-02`, `RL-03`).
4. Add daemon crash-transition and 20-cycle lifecycle tests (`RL-04`, `RL-09`).
5. Run focused crate tests, then full `orchestrator-core` test pass.

## Risks and Mitigations
- Risk: lifecycle tests depend on host runner state.
  - Mitigation: force isolated temp config roots and explicit env overrides.
- Risk: timing flakiness in readiness/shutdown loops.
  - Mitigation: use deterministic seams or short, bounded waits in tests.
- Risk: accidental behavior changes while adding seams.
  - Mitigation: keep seams internal and assert no API/contract changes.

## Validation Targets
- `cargo test -p orchestrator-core runner_helpers`
- `cargo test -p orchestrator-core services::tests`
- `cargo test -p orchestrator-core`
