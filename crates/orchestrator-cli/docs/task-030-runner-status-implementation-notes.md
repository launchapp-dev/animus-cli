# TASK-030 Implementation Notes: Runner Exit Status Propagation and Status Lookup Not-Found Handling

## Purpose
Translate TASK-030 requirements into a minimal, deterministic implementation
slice that fixes runner status truthfulness and preserves CLI not-found
semantics.

## Non-Negotiable Constraints
- Keep changes scoped to Rust crates in this repository.
- Do not manually edit `.ao/*.json`.
- Preserve existing behavior outside status-query and cleanup status propagation
  surfaces.
- Keep protocol changes additive and parse-compatible for updated components.

## Proposed Change Surface

### 1) Protocol status-query response envelope
- `crates/protocol/src/agent_runner.rs`
  - add a tagged status-query response enum with success and error variants.
  - define a typed status-query error payload/code for not-found lookups.
  - keep `AgentStatusResponse` unchanged to preserve existing success payload
    semantics.
- `crates/protocol/tests/agent_runner.rs`
  - add serialization roundtrip tests for new status-query success/error
    variants.

### 2) Runner cleanup status propagation
- `crates/agent-runner/src/runner/mod.rs`
  - replace cleanup channel payload from `RunId` to a small struct carrying
    `{ run_id, terminal_status }`.
  - update `cleanup_agent` to persist provided terminal status (instead of
    always `Completed`).
  - keep guard behavior that only writes finished status when run is still in
    `running_agents`.
- `crates/agent-runner/src/ipc/server.rs`
  - update cleanup consumer loop to pass status-aware cleanup payload.
- `crates/agent-runner/src/runner/supervisor.rs`
  - return or otherwise surface deterministic terminal status outcome from
    `spawn_agent` execution path so caller can enqueue cleanup with status.

### 3) Unknown-run status lookup behavior
- `crates/agent-runner/src/runner/mod.rs`
  - change status query return surface from synthetic fallback success payload to
    typed not-found error response for missing run IDs.
- `crates/agent-runner/src/ipc/handlers/status.rs`
  - serialize status-query union response (success or error) instead of a
    success-only payload.

### 4) CLI status-query parsing and fallback boundary
- `crates/orchestrator-cli/src/services/runtime/runtime_agent/status.rs`
  - parse new status-query response enum.
  - convert typed `not_found` status error into an anyhow not-found message
    (`run not found: <run_id>`) so output classifier maps it to exit code `3`.
  - preserve event-log fallback for transport/no-response errors only.

### 5) Regression tests
- `crates/agent-runner/src/runner/mod.rs` (or adjacent runner test module)
  - cleanup terminal status propagation tests.
  - unknown-run typed not-found status query tests.
- `crates/orchestrator-cli/src/services/runtime/runtime_agent/status.rs`
  - parser/fallback behavior tests for typed not-found vs transport failure.
- `crates/protocol/tests/agent_runner.rs`
  - status-query response roundtrip tests.

## Deterministic Behavior Rules To Preserve
- `exit_code == 0` => `AgentStatus::Completed`.
- `exit_code != 0` => `AgentStatus::Failed`.
- supervisor/process errors without exit code => `AgentStatus::Failed`.
- `stop_agent` still produces/stores `AgentStatus::Terminated` and is not
  overwritten by late cleanup.
- unknown run ID status query => typed `not_found` response (no fabricated
  timestamp fields).

## Implementation Sequence
1. Add protocol types/tests for status-query success/error responses.
2. Wire runner cleanup message type and terminal-status propagation.
3. Update status lookup path in runner + IPC handler to emit typed responses.
4. Update CLI parser/fallback boundary for typed not-found handling.
5. Add targeted tests across protocol, runner, and CLI parsing surfaces.
6. Run targeted test commands and address regressions introduced by this change.

## Risks and Mitigations
- Risk: transport fallback in CLI still masks typed not-found.
  - Mitigation: treat typed not-found as handled application response, not a
    transport/query failure.
- Risk: stop/cleanup race causes status overwrite.
  - Mitigation: preserve existing `running_agents.remove(...)` guard semantics.
- Risk: shape mismatch during rollout between runner and CLI versions.
  - Mitigation: keep parser tolerant for success payload where needed and fail
    with explicit compatibility errors for malformed responses.

## Validation Targets
- `cargo test -p protocol agent_runner`
- `cargo test -p agent-runner runner::`
- `cargo test -p orchestrator-cli runtime_agent`
