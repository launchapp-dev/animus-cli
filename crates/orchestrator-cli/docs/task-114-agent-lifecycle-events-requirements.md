# TASK-114 Requirements: Emit Real-Time Agent Lifecycle Events for Pool Visibility

## Phase
- Workflow phase: `requirements`
- Workflow ID: `ec689242-fd01-4485-ac6b-7133409729be`
- Task: `TASK-114`

## Objective
Emit daemon events for individual agent lifecycle transitions to provide real-time pool visibility for the web UI and observability tooling.

## Current Baseline Audit
Snapshot date: `2026-02-28`.

| Surface | Current location | Current behavior | Gap |
| --- | --- | --- | --- |
| Agent pool | `daemon_agent_pool.rs` (`AgentPool`) | Tracks spawn/completion/fail counts internally | No events emitted for pool state changes |
| Phase execution | `daemon_scheduler_phase_exec.rs` (`run_workflow_phase`) | Returns `PhaseExecutionOutcome` with results | No daemon event emission on phase start/complete/fail |
| Pool backfill | Implicit in tick loop | Backfill happens on next tick after completion | No explicit backfill event emitted |
| Pool capacity | `ready_task_dispatch_limit()` in project tick | Limits dispatch based on max_agents | No "pool-full" event when work available but pool at capacity |

## Problem Statement
The daemon currently provides aggregate queue and workflow events but lacks granular agent-level lifecycle events. This prevents the web UI from displaying real-time agent pool status and blocks future SSE/WebSocket streaming for live updates.

## Scope
In scope for implementation after this phase:
- Add daemon events for agent lifecycle transitions:
  - `agent-spawned`: emitted when an agent slot starts executing a workflow phase
  - `agent-completed`: emitted when an agent successfully completes a phase
  - `agent-failed`: emitted when an agent fails or errors during phase execution
  - `pool-backfill`: emitted when the pool is refilled after agent completion
  - `pool-full`: emitted when work is available but pool is at capacity
- Events must include task_id, workflow_id, phase_id for traceability
- Events must include pool metrics (pool_active, pool_size) for visibility
- Events flow through existing `emit_daemon_event` infrastructure
- Events visible via `ao daemon events` command

Out of scope:
- SSE/WebSocket server implementation (future work)
- Changes to workflow phase semantics
- Non-deterministic behavior changes

## Constraints
- Existing daemon event schema must remain backward-compatible (additive only)
- Events must be deterministic for identical workflow snapshots
- Pool metrics must be accurate at emission time
- Event emission must not block agent execution

## Event Schema Contracts

### agent-spawned
```json
{
  "event_type": "agent-spawned",
  "data": {
    "task_id": "TASK-001",
    "workflow_id": "WF-xxx",
    "phase_id": "implementation",
    "pool_active": 3,
    "pool_size": 5
  }
}
```

### agent-completed
```json
{
  "event_type": "agent-completed",
  "data": {
    "task_id": "TASK-001",
    "workflow_id": "WF-xxx",
    "phase_id": "implementation",
    "duration_secs": 42,
    "outcome": "advance",
    "pool_active": 2,
    "pool_size": 5
  }
}
```
- `outcome`: one of "advance", "rework", "fail"

### agent-failed
```json
{
  "event_type": "agent-failed",
  "data": {
    "task_id": "TASK-001",
    "workflow_id": "WF-xxx",
    "phase_id": "implementation",
    "error": "phase execution failed: ...",
    "pool_active": 2,
    "pool_size": 5
  }
}
```

### pool-backfill
```json
{
  "event_type": "pool-backfill",
  "data": {
    "task_id": "TASK-002",
    "workflow_id": "WF-xxx",
    "triggered_by": "agent-completion"
  }
}
```
- `triggered_by`: one of "agent-completion", "housekeeping"

### pool-full
```json
{
  "event_type": "pool-full",
  "data": {
    "queued_count": 10,
    "active_count": 5,
    "pool_size": 5
  }
}
```

## Functional Requirements

### FR-01: Agent Spawned Event
When an agent slot begins executing a workflow phase, emit an `agent-spawned` event containing:
- task_id, workflow_id, phase_id
- Current pool_active count and pool_size

### FR-02: Agent Completed Event
When an agent successfully completes a workflow phase, emit an `agent-completed` event containing:
- task_id, workflow_id, phase_id
- duration_secs (execution time)
- outcome ("advance", "rework", or as determined by phase decision)
- Current pool_active count and pool_size

### FR-03: Agent Failed Event
When an agent fails or errors during workflow phase execution, emit an `agent-failed` event containing:
- task_id, workflow_id, phase_id
- Error message/description
- Current pool_active count and pool_size

### FR-04: Pool Backfill Event
When the pool is refilled after agent completion (either via completion path or housekeeping), emit a `pool-backfill` event containing:
- task_id (the newly started task, if any)
- workflow_id
- triggered_by ("agent-completion" or "housekeeping")

### FR-05: Pool Full Event
When work is available but the pool is at capacity, emit a `pool-full` event containing:
- queued_count (work items waiting)
- active_count (currently running)
- pool_size (maximum capacity)

This event should be emitted once per condition occurrence to avoid spam.

### FR-06: Event Visibility
All events must:
- Flow through existing `emit_daemon_event` infrastructure
- Be visible via `ao daemon events` command
- Include appropriate project_root for filtering

## Implementation Notes

### Agent Pool Integration
The `AgentPool` in `daemon_agent_pool.rs` is where agent slots are spawned and completed:
- `spawn_with_permit()`: internal method that spawns agents - ideal location for `agent-spawned` event
- Completion is handled internally in the spawned task - need to emit events from the completion callback or at the call site

### Phase Execution Integration
Phase execution happens in `daemon_scheduler_phase_exec.rs`:
- `run_workflow_phase()`: executes a single phase, returns `PhaseExecutionRunResult`
- `PhaseExecutionOutcome`: contains `Completed`, `NeedsResearch`, `Failed` variants
- These are the perfect emission points for `agent-completed` and `agent-failed` events

### Pool State Tracking
Current pool metrics tracking:
- `AgentPool::active_count()`: returns current active agent count
- `AgentPool::is_full()`: returns whether pool is at capacity
- Need pool_size from daemon health or config

### Event Emission Points
Suggested implementation locations:
1. `agent-spawned`: In the phase execution spawn location (around line 1364 in project_tick.rs where `run_workflow_phase_with_agent` is called)
2. `agent-completed`/`agent-failed`: In `process_phase_execution_completion` around line 1399 in project_tick.rs
3. `pool-backfill`: At the same location as completion handling when new work is started
4. `pool-full`: In `ready_task_dispatch_limit` when dispatch is limited by pool capacity

## Acceptance Criteria

1. **Event Emission**: All five event types are emitted at appropriate times
2. **Data Accuracy**: Events contain accurate task_id, workflow_id, phase_id, and pool metrics
3. **Infrastructure**: Events flow through existing `emit_daemon_event` path
4. **Visibility**: Events appear in `ao daemon events` output
5. **No Regression**: Existing daemon behavior and events remain unchanged
6. **Test Coverage**: New events have corresponding test coverage

## Risk Assessment

- **Low Risk**: Events are additive and don't change existing behavior
- **Medium Risk**: Event emission timing must not block agent execution
- **Mitigation**: Emit events asynchronously where possible, use non-blocking patterns
