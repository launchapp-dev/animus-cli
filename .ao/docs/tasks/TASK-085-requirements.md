# TASK-085: Queue Management REST API - Requirements Analysis

## Task Description
Expose the EM work queue through REST endpoints. GET /api/queue for ordered work items. POST /api/queue/reorder for manual reprioritization. GET /api/queue/stats for queue depth, throughput, and wait time metrics. POST /api/queue/hold/:task_id and POST /api/queue/release/:task_id for manual holds.

## Implementation Status

### ✅ Completed Endpoints
| Endpoint | Method | Path | Status |
|----------|--------|------|--------|
| Queue List | GET | `/api/v1/queue` | Implemented |
| Queue Reorder | POST | `/api/v1/queue/reorder` | Implemented |
| Queue Hold | POST | `/api/v1/queue/hold/{task_id}` | Implemented |
| Queue Release | POST | `/api/v1/queue/release/{task_id}` | Implemented |

### ⚠️ Partial Implementation
| Endpoint | Method | Path | Status |
|----------|--------|------|--------|
| Queue Stats | GET | `/api/v1/queue/stats` | Partially implemented |

## Queue Stats Gap Analysis

### Current Response
```json
{
  "depth": 5,
  "pending": 3,
  "assigned": 1,
  "held": 1,
  "ready": 3
}
```

### Required Response (per task description)
```json
{
  "depth": 5,
  "pending": 3,
  "assigned": 1,
  "held": 1,
  "ready": 3,
  "throughput": 2.5,
  "avg_wait_time_seconds": 120
}
```

### Required Changes
1. **Add `queued_at` field** to `EmWorkQueueEntry` - timestamp when task entered queue
2. **Track completed tasks** - maintain history or counter for throughput calculation  
3. **Compute wait time** - calculate time from `queued_at` to `assigned_at`

## Acceptance Criteria

### Must Have
- [x] GET /api/queue returns ordered work items
- [x] POST /api/queue/reorder reprioritizes queue
- [x] POST /api/queue/hold/:task_id holds a pending task
- [x] POST /api/queue/release/:task_id releases a held task
- [x] GET /api/queue/stats returns depth, pending, assigned, held counts

### Should Have (Gap)
- [ ] GET /api/queue/stats returns throughput metric (tasks/hour)
- [ ] GET /api/queue/stats returns average wait time metric

## Technical Constraints

1. **No external metrics system** - All metrics must be derived from queue state
2. **Minimal persistence changes** - Avoid schema migrations if possible
3. **Backward compatible** - New stats fields should be optional

## Recommendation

The core endpoints are implemented and functional. The throughput/wait time metrics require:
- Schema change to add `queued_at` to queue entries
- New computation logic in `queue_stats()`
- Potential history tracking for historical throughput

**Recommendation**: Proceed with implementation phase to add the missing throughput and wait time metrics, OR clarify if current stats are sufficient for the dashboard.
