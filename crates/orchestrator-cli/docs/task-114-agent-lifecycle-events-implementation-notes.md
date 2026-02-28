# TASK-114 Implementation Notes: Emit Real-Time Agent Lifecycle Events for Pool Visibility

## Phase: requirements
## Date: 2026-02-28

## Implementation Approach

### Architecture Summary
The daemon already has a mature event emission system:
1. `PhaseExecutionEvent` structs are created in `daemon_scheduler_project_tick.rs`
2. These are collected in `ProjectTickSummary::phase_execution_events`  
3. Emitted as daemon events in `daemon_run.rs` via `emit_project_tick_summary_events`

### Key Integration Points

#### 1. Agent Pool State (daemon_agent_pool.rs)
- `AgentPool` tracks `active_count`, `total_spawned`, `total_completed`, `total_failed` via atomics
- Pool size needs to be passed in or tracked separately (from `max_agents` config)

#### 2. Phase Execution Flow (daemon_scheduler_project_tick.rs)
- `execute_running_workflow_phases_for_project` spawns phases via `run_workflow_phase_with_agent`
- `process_phase_execution_completion` handles outcomes (Completed, NeedsResearch, Failed)
- Both functions return/fill `PhaseExecutionEvent` vectors

#### 3. Reactive Pool Coordinator (daemon_scheduler_project_tick.rs)
- `ReactivePhasePoolState` manages per-project reactive pools
- Completion channel `completion_tx` sends `ReactivePhaseCompletion` on phase finish
- Backfill happens when completion is processed

### Event Emission Locations

| Event | Location | Data Sources |
|-------|----------|--------------|
| `agent-spawned` | In `execute_running_workflow_phases_for_project` around line 1364 where `run_workflow_phase_with_agent` is called | task_id, workflow_id, phase_id from `scheduled`, pool metrics |
| `agent-completed` | In `process_phase_execution_completion` when outcome is `Completed` | task_id, workflow_id, phase_id, duration, outcome, pool metrics |
| `agent-failed` | In `process_phase_execution_completion` when outcome is `Failed` | task_id, workflow_id, phase_id, error, pool metrics |
| `pool-backfill` | In reactive pool completion handler when new work starts | task_id, workflow_id, triggered_by |
| `pool-full` | In `ready_task_dispatch_limit` when work available but pool at capacity | queued_count, active_count, pool_size |

### Implementation Steps

1. **Add pool metrics helper**: Create function to get current pool metrics (active_count, pool_size) for a project
2. **Extend PhaseExecutionEvent**: Add optional fields for duration_secs, outcome, error as needed
3. **Emit events at spawn**: Add `agent-spawned` event in phase spawn location
4. **Emit events at completion**: Enhance `process_phase_execution_completion` to emit `agent-completed`/`agent-failed`
5. **Emit pool-backfill**: Track when backfill occurs and emit event
6. **Emit pool-full**: Add condition check in dispatch limiting logic

### Pool Metrics Access
- Active count: `AgentPool::active_count()` (but pool is not directly accessible from project_tick)
- Pool size: From `daemon health` -> `max_agents` or from project config
- Need to pass pool reference or metrics through the execution context

### Backward Compatibility
- All new events are additive to existing event types
- Existing `emit_daemon_event` path remains unchanged
- No breaking changes to event schema

### Testing Approach
1. Unit tests for event construction
2. Integration tests for event visibility via `ao daemon events`
3. Verify event data accuracy against pool state

## Files to Modify
1. `daemon_scheduler.rs` - Add event types/constants if needed
2. `daemon_scheduler_project_tick.rs` - Add event emission calls
3. Possibly `daemon_agent_pool.rs` - Add event emission callbacks or pass metrics

## Dependencies
- Uses existing `emit_daemon_event_with_notifications` infrastructure
- No new dependencies required
