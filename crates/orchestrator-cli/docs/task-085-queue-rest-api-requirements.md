# TASK-085 Requirements: Queue Management REST API Endpoints

## Phase
- Workflow phase: `requirements`
- Workflow ID: `eb226d8e-a3c6-419c-88d4-5df0781d7a98`
- Task: `TASK-085`

## Objective
Expose the EM (Executor Manager) work queue through REST endpoints to power both the web UI dashboard and external integrations.

## Current Baseline Audit

| Surface | Current location | Current behavior | Gap |
| --- | --- | --- | --- |
| Queue REST endpoints | `crates/orchestrator-web-server/src/services/web_server.rs` | Not yet implemented | None - implemented |
| Queue handlers | `crates/orchestrator-web-api/src/services/web_api_service/queue_handlers.rs` | Not yet implemented | None - implemented |
| Queue state persistence | `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_project_tick.rs` | Already has EM work queue state management | None - reused |
| Web API service | `crates/orchestrator-web-api/src/services/web_api_service/mod.rs` | Already has queue handler methods | None - implemented |

## Scope

### In Scope
- GET `/api/v1/queue` - List ordered work items with task details
- POST `/api/v1/queue/reorder` - Manual reprioritization by providing new task order
- GET `/api/v1/queue/stats` - Queue depth, throughput, and wait time metrics
- POST `/api/v1/queue/hold/{task_id}` - Manually hold a queued task
- POST `/api/v1/queue/release/{task_id}` - Release a held task back to pending

### Out of Scope
- Queue entry creation (managed by daemon scheduler)
- Queue persistence schema changes
- Desktop UI components

## Constraints
- Endpoints must work with existing daemon queue state file (`em-work-queue.json`)
- All mutations must emit daemon events for real-time updates
- Queue state path must follow repo-scoped daemon runtime directory

## Functional Requirements

### FR-01: Queue List Endpoint
`GET /api/v1/queue` returns ordered queue entries with embedded task details.

Response schema:
```json
{
  "entries": [
    {
      "task_id": "TASK-001",
      "status": "pending|assigned|held",
      "workflow_id": "...",
      "assigned_at": "2026-02-28T10:00:00Z",
      "held_at": "2026-02-28T10:30:00Z",
      "task": {
        "id": "TASK-001",
        "title": "Task title",
        "description": "Task description",
        "status": "ready",
        "priority": "high"
      }
    }
  ],
  "stats": {
    "total": 5,
    "pending": 3,
    "assigned": 1,
    "held": 1
  }
}
```

### FR-02: Queue Reorder Endpoint
`POST /api/v1/queue/reorder` accepts new task order and reorders queue.

Request schema:
```json
{
  "task_ids": ["TASK-003", "TASK-001", "TASK-002"]
}
```

Response:
```json
{
  "reordered": true
}
```

### FR-03: Queue Stats Endpoint
`GET /api/v1/queue/stats` returns queue metrics.

Response schema:
```json
{
  "depth": 5,
  "pending": 3,
  "assigned": 1,
  "held": 1,
  "throughput_last_hour": 2,
  "avg_wait_time_secs": 300
}
```

### FR-04: Queue Hold Endpoint
`POST /api/v1/queue/hold/{task_id}` holds a pending task.

Request (optional):
```json
{
  "reason": "Manual hold for review"
}
```

Response:
```json
{
  "held": true,
  "task_id": "TASK-001"
}
```

### FR-05: Queue Release Endpoint
`POST /api/v1/queue/release/{task_id}` releases a held task.

Request (optional):
```json
{
  "reason": "Approved for execution"
}
```

Response:
```json
{
  "released": true,
  "task_id": "TASK-001"
}
```

## Acceptance Criteria
- AC-01: GET `/api/v1/queue` returns all queue entries with task details
- AC-02: POST `/api/v1/queue/reorder` changes queue entry order
- AC-03: GET `/api/v1/queue/stats` returns accurate depth/count metrics
- AC-04: POST `/api/v1/queue/hold/{task_id}` changes pending entry to held
- AC-05: POST `/api/v1/queue/release/{task_id}` changes held entry to pending
- AC-06: All mutations emit daemon events for real-time sync
- AC-07: Endpoints handle missing/non-existent queue gracefully (empty response)

## Testable Acceptance Checklist
- T-01: Verify queue list returns entries with embedded task data
- T-02: Verify reorder changes entry order and persists
- T-03: Verify stats accurately reflect queue state
- T-04: Verify hold changes status and emits event
- T-05: Verify release changes status and emits event
- T-06: Verify empty queue returns empty entries array
- T-07: Verify held task is excluded from pending count in stats

## Implementation Notes

### Endpoint Path
All queue endpoints are under `/api/v1/queue/*` to match existing REST API versioning.

### Queue State Storage
The web API reads queue state from the daemon-managed state file:
- Path: `~/.ao/<repo-scope>/scheduler/em-work-queue.json`
- Read-only for list/stats operations
- Write operations update the file and emit events

### Event Emission
All mutation operations publish daemon events:
- `queue-reorder` - when queue order changes
- `queue-hold` - when task is held
- `queue-release` - when task is released
