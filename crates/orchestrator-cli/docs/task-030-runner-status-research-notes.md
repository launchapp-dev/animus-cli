# TASK-030 Research Notes: Runner Exit Status + Not-Found Status Queries

## Scope
- Workflow: `e4c951f5-3c9d-4882-8c13-5d2244e80a6e`
- Task: `TASK-030` (`in-progress`)
- Requirement: `REQ-003` (`draft`)
- Objective:
  - propagate actual run terminal status through `Runner::cleanup_agent`
  - return a distinct not-found outcome for unknown run IDs in status lookup

## AO State Evidence (2026-02-26)
- Task definition explicitly calls out both defects and links `REQ-003`:
  - `.ao/tasks/TASK-030.json:15`
  - `.ao/tasks/TASK-030.json:20`
- Requirement scope is runner reliability/diagnostics:
  - `.ao/requirements/generated/REQ-003.json:12`
  - `.ao/requirements/generated/REQ-003.json:34`
- Noted traceability gap: `REQ-003` currently links `TASK-003` only, not `TASK-030`:
  - `.ao/requirements/generated/REQ-003.json:19`

## Code Evidence

### 1. Cleanup path discards terminal outcome
- `Runner` tracks cleanup channel as `mpsc::Sender<RunId>`.
  - `crates/agent-runner/src/runner/mod.rs:30`
- Run completion task enqueues only `run_id`.
  - `crates/agent-runner/src/runner/mod.rs:106`
- `cleanup_agent` unconditionally writes `AgentStatus::Completed`.
  - `crates/agent-runner/src/runner/mod.rs:112`
  - `crates/agent-runner/src/runner/mod.rs:119`
- IPC cleanup consumer also receives `RunId` only.
  - `crates/agent-runner/src/ipc/server.rs:38`
  - `crates/agent-runner/src/ipc/server.rs:43`

### 2. Exit status is available before cleanup
- Supervisor receives concrete process exit code on success and emits `Finished { exit_code }`.
  - `crates/agent-runner/src/runner/supervisor.rs:136`
  - `crates/agent-runner/src/runner/supervisor.rs:150`
  - `crates/agent-runner/src/runner/supervisor.rs:154`
- Process layer returns `Ok(exit_code)` for completed process execution.
  - `crates/agent-runner/src/runner/process.rs:1008`
  - `crates/agent-runner/src/runner/process.rs:1009`

### 3. Unknown run IDs currently fabricate failure state
- Status lookup falls back to synthetic `Failed` with generated timestamps when run ID is unknown.
  - `crates/agent-runner/src/runner/mod.rs:185`
  - `crates/agent-runner/src/runner/mod.rs:188`
  - `crates/agent-runner/src/runner/mod.rs:190`
- IPC status handler always serializes a success-shaped `AgentStatusResponse`.
  - `crates/agent-runner/src/ipc/handlers/status.rs:59`
  - `crates/agent-runner/src/ipc/handlers/status.rs:66`

### 4. Status query protocol has no typed not-found variant today
- Protocol defines `AgentStatusResponse` only for status queries.
  - `crates/protocol/src/agent_runner.rs:87`
- Runner currently does not use `protocol::ProtocolError` in this flow.
  - `crates/protocol/src/errors.rs:6`
- CLI status query parser accepts only `AgentStatusResponse`.
  - `crates/orchestrator-cli/src/services/runtime/runtime_agent/status.rs:167`

### 5. CLI fallback and exit-code behavior constraints
- CLI falls back to event-log status only when runner query returns an error.
  - `crates/orchestrator-cli/src/services/runtime/runtime_agent/status.rs:136`
  - `crates/orchestrator-cli/src/services/runtime/runtime_agent/status.rs:139`
- Fallback returns not-found-style error when no run event log exists.
  - `crates/orchestrator-cli/src/shared/parsing.rs:82`
- CLI envelope mapping treats `"not found"` text as exit code `3`.
  - `crates/orchestrator-cli/src/shared/output.rs:13`
  - `crates/orchestrator-cli/src/shared/output.rs:79`

### 6. Coverage gap on affected behavior
- There are no runner unit tests exercising `cleanup_agent` or unknown-run status responses.
  - `crates/agent-runner/src/runner/mod.rs` (no `#[cfg(test)]` section)
- `protocol` tests cover only `AgentStatusResponse` roundtrips; no status-error payload exists yet.
  - `crates/protocol/tests/agent_runner.rs:44`
- CLI status code has no tests for runner returning a structured not-found response.
  - `crates/orchestrator-cli/src/services/runtime/runtime_agent/status.rs:146`

## Assumptions
- `exit_code == 0` maps to `AgentStatus::Completed`.
- `exit_code != 0` maps to `AgentStatus::Failed`.
- Process/supervisor errors without exit code map to `AgentStatus::Failed`.
- `stop_agent`-set `AgentStatus::Terminated` remains authoritative and must not be overwritten by async cleanup.
- Unknown run ID is a lookup miss, not an execution failure.

## Deterministic Interface Proposal
- Runner-internal cleanup channel payload:
  - `struct CleanupMessage { run_id: RunId, terminal_status: AgentStatus }`
- Status query wire response (new protocol enum):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentStatusQueryResponse {
    Status(AgentStatusResponse),
    Error(AgentStatusErrorResponse),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStatusErrorResponse {
    pub run_id: RunId,
    pub code: AgentStatusErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatusErrorCode {
    NotFound,
}
```

- Fallback behavior change for CLI:
  - keep event-log fallback on transport failures/no response
  - do **not** hide a typed runner `not_found` response behind event-log fallback

## Build-Ready Implementation Plan
1. Add protocol response types first (`crates/protocol/src/agent_runner.rs`, `crates/protocol/tests/agent_runner.rs`).
- Introduce `AgentStatusQueryResponse`, `AgentStatusErrorResponse`, and `AgentStatusErrorCode`.
- Preserve backward compatibility by keeping `AgentStatusResponse` unchanged.

2. Propagate terminal status through runner cleanup (`crates/agent-runner/src/runner/mod.rs`, `crates/agent-runner/src/ipc/server.rs`).
- Replace `mpsc::Sender<RunId>` with `mpsc::Sender<CleanupMessage>`.
- Update cleanup consumer and `cleanup_agent` to write the provided terminal status.
- Keep stop-race safety by only inserting into `finished_agents` if run is still active.

3. Return terminal status from supervisor path (`crates/agent-runner/src/runner/supervisor.rs` and caller in `runner/mod.rs`).
- Make `Supervisor::spawn_agent` return `AgentStatus`.
- Map process result deterministically:
  - `Ok(0)` => `Completed`
  - `Ok(non_zero)` => `Failed`
  - `Err(_)` => `Failed`
  - workspace validation failure => `Failed`

4. Emit typed status-query responses (`crates/agent-runner/src/runner/mod.rs`, `crates/agent-runner/src/ipc/handlers/status.rs`).
- Change status lookup to return `Result<AgentStatusResponse, AgentStatusErrorResponse>`.
- Serialize success/error through `AgentStatusQueryResponse`.

5. Parse dual shapes in CLI and preserve not-found semantics (`crates/orchestrator-cli/src/services/runtime/runtime_agent/status.rs`).
- Parse `AgentStatusQueryResponse` instead of only `AgentStatusResponse`.
- Convert typed not-found status error into `anyhow!("run not found: ...")`.
- Do not event-log-fallback when runner explicitly returns typed not-found.

6. Add targeted regression tests.
- `agent-runner`: cleanup status propagation + unknown-run typed error.
- `protocol`: status-query response roundtrip (success + error variants).
- `orchestrator-cli`: typed not-found parse path yields `not_found`-classifiable error.

## Risks And Mitigations
- Risk: new runner status-error payload is ignored by older CLI parsing.
- Mitigation: update parser to check both shapes and fail fast on unknown non-empty lines.

- Risk: race between `stop_agent` and cleanup writeback.
- Mitigation: retain current guard behavior (cleanup mutates only when run still present in `running_agents`).

- Risk: protocol shape changes regress compatibility silently.
- Mitigation: add explicit protocol serialization tests before wiring handlers.

## External Blockers
- None. First-party code and active task/requirement artifacts are sufficient for implementation.

## Validation Plan (Implementation Phase)
- `cargo test -p agent-runner runner::`
- `cargo test -p protocol agent_runner`
- `cargo test -p orchestrator-cli runtime_agent`
