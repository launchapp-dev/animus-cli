# TASK-085: Queue Management REST API — UX Brief

## Phase: ux-research

---

## 1. Context & User Roles

The queue REST API serves two consumer types:

| Consumer | Mode | Primary Need |
|----------|------|--------------|
| Web UI dashboard | Visual, interactive | Real-time queue visibility, drag-to-reorder, hold/release per item |
| External integrations (CI, scripts) | Programmatic | Stable JSON contract, reliable status transitions, metrics for alerting |

Both consumers share the same endpoints. API contract changes affect both simultaneously.

---

## 2. Current Endpoint Inventory

| Route | Method | Status | Notes |
|-------|--------|--------|-------|
| `/api/v1/queue` | GET | ✅ Implemented | Returns `{entries, total}` |
| `/api/v1/queue/stats` | GET | ⚠️ Partial | Missing `throughput`, `avg_wait_time_seconds` |
| `/api/v1/queue/reorder` | POST | ✅ Implemented | Body: `{task_ids: [...]}` |
| `/api/v1/queue/hold/{task_id}` | POST | ✅ Implemented | Body: `{reason?: string}` |
| `/api/v1/queue/release/{task_id}` | POST | ✅ Implemented | Body: `{}` |

---

## 3. Key Screens & Data Flows

### 3.1 Queue Dashboard (Main Screen)

**Purpose**: Provide a live view of the execution queue ordered by priority.

**Data source**: `GET /api/v1/queue` (poll or SSE-refresh)

**Columns needed** (in priority order):
1. Position (1-indexed, mutable via drag)
2. Task ID (linkable to task detail)
3. Status badge: `pending` / `assigned` / `held` / `unknown`
4. Time in queue — derived from `queued_at` (currently **not in the response**; must be added)
5. Assigned workflow ID (shown when status = `assigned`)
6. Hold reason (shown when status = `held`)
7. Actions: Hold / Release (contextual by status)

**Current response shape** (per entry):
```json
{
  "task_id": "TASK-085",
  "status": "pending",
  "workflow_id": null,
  "assigned_at": "2026-02-28T12:00:00Z",
  "held_at": null,
  "hold_reason": null
}
```

**Missing field for wait-time display**: `queued_at` timestamp. Without it, the UI cannot show time-in-queue or compute avg_wait_time_seconds.

---

### 3.2 Queue Stats Panel

**Purpose**: At-a-glance operational health.

**Data source**: `GET /api/v1/queue/stats` (poll, suggested 5s interval)

**Current response**:
```json
{ "depth": 5, "pending": 3, "assigned": 1, "held": 1, "ready": 3 }
```

**Required additions** (per task description):
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

**Display layout** (stat card strip):
```
┌──────────┬──────────┬──────────┬──────────┬─────────────┬──────────────────┐
│  Depth   │ Pending  │ Assigned │   Held   │ Throughput  │  Avg Wait Time   │
│    5     │    3     │    1     │    1     │  2.5/hr     │    2 min 0 sec   │
└──────────┴──────────┴──────────┴──────────┴─────────────┴──────────────────┘
```

Throughput and avg_wait_time are **optional** in the response; display "—" when absent.

---

### 3.3 Hold / Release Interaction

**Trigger**: Per-row action button in queue list.

**State machine**:
```
pending ──[Hold]──> held ──[Release]──> pending
assigned (no hold allowed — API returns 409 conflict)
```

**Hold flow**:
1. User clicks "Hold" → inline reason text input appears
2. User types optional reason → clicks "Confirm Hold"
3. `POST /api/v1/queue/hold/{task_id}` with `{ reason: "..." }`
4. On success: row badge changes to `held`, action changes to "Release"
5. On 409 (already assigned): show inline error "Task is currently being executed"

**Release flow**:
1. User clicks "Release" → immediate action (no confirmation needed; hold is reversible)
2. `POST /api/v1/queue/release/{task_id}` with `{}`
3. On success: row badge changes to `pending`, action changes to "Hold"

---

### 3.4 Queue Reorder Interaction

**Trigger**: Drag-and-drop row handle OR explicit "Move to position N" input.

**API call**: `POST /api/v1/queue/reorder` with `{ task_ids: ["TASK-A", "TASK-B", ...] }`

**Behavior**:
- `task_ids` in body defines the NEW order for those items
- Tasks NOT in `task_ids` are appended after, preserving their relative order
- Assigned and held tasks CAN be included in reorder (they keep their status)

**UX constraint**: Reorder should be optimistic (update UI immediately, rollback on error).

---

## 4. Data Model Gaps

### 4.1 Missing `queued_at` Field

The `EmWorkQueueEntry` struct in the daemon (`daemon_scheduler_project_tick.rs:150`) does not write a `queued_at` timestamp. The web API struct (`queue_handlers.rs:33`) also lacks it.

Without `queued_at`, the following features cannot be supported:
- "Time in queue" column in the UI
- `avg_wait_time_seconds` in stats
- Historical throughput calculation

**Required schema addition**:
```rust
#[serde(default)]
queued_at: Option<String>,  // ISO 8601 RFC3339, set when entry is first created
```

Both the daemon writer (`save_em_work_queue_state`) and the web API reader must handle old entries gracefully via `#[serde(default)]`.

### 4.2 Throughput Computation Strategy

Without a persistent completion log, throughput must be approximated from current state. Options:

| Strategy | Accuracy | Persistence change |
|----------|----------|--------------------|
| Count assigned tasks transitioning out per interval (in-memory, lost on restart) | Low | None |
| Track `completed_count` + window in queue state file | Medium | Minor (add 2 fields to `EmWorkQueueState`) |
| Full completion history log | High | New file |

**Recommendation**: Add `completed_in_last_hour` counter to `EmWorkQueueState` (incremented by daemon when removing terminal entries), reset hourly. This is minimal and survives restarts.

---

## 5. Accessibility Constraints

### Status Badges
- Must meet WCAG AA contrast (4.5:1 for text)
- Do not rely on color alone — include status text or icon
  - `pending` → clock icon + "Pending"
  - `assigned` → spinner icon + "Assigned"
  - `held` → pause icon + "Held"
  - `unknown` → warning icon + "Unknown"

### Keyboard Navigation
- Queue rows must be focusable (`tabindex="0"`)
- Hold/Release buttons must be reachable via Tab
- Drag-to-reorder must have a keyboard alternative (e.g., arrow keys while row is focused, or "Move up/down" buttons)
- Confirm Hold dialog must trap focus and return focus on close

### Screen Reader Labels
- Status badges: `aria-label="Status: pending"` (not just the icon)
- Action buttons: `aria-label="Hold TASK-085"` (include task ID to disambiguate rows)
- Stats panel values: each card should have a `<dt>` label + `<dd>` value structure

### Live Region for Polling Updates
- Stats panel should use `aria-live="polite"` so screen readers announce value changes without interrupting
- Queue list should NOT use `aria-live` (too noisy); instead expose a "last updated" timestamp

---

## 6. Error States to Handle

| Scenario | API response | UI behavior |
|----------|-------------|-------------|
| Hold a non-pending task | 409 `conflict` | Inline error: "Task cannot be held in current state" |
| Release a non-held task | 409 `conflict` | Inline error: "Task is not currently held" |
| Task not found in queue | 404 `not_found` | Toast: "Task not found in queue" |
| Queue file unreadable | 500 `internal` | Error state with retry button |
| Reorder with unknown task_id | 200 (silently skipped) | No special handling needed; unknown IDs are ignored |

---

## 7. Interaction Summary

```
Queue Dashboard
├── Stats Strip (polled 5s)
│   ├── depth, pending, assigned, held
│   └── throughput, avg_wait [optional, show "—" if absent]
│
├── Queue List (polled 2s or SSE-triggered)
│   ├── Drag handle (reorder)
│   ├── Position number
│   ├── Task ID (link to /tasks/{id})
│   ├── Status badge (color + icon + text)
│   ├── Time in queue [requires queued_at field]
│   ├── Assigned workflow (if assigned)
│   └── Hold/Release action (contextual)
│       ├── Hold → reason input → confirm
│       └── Release → immediate
│
└── Reorder
    ├── Drag rows to new position
    └── POST /api/v1/queue/reorder on drop
```

---

## 8. Implementation Notes for Next Phase

The `impl` phase should address these in priority order:

1. **Add `queued_at` to `EmWorkQueueEntry`** in both `daemon_scheduler_project_tick.rs` and `queue_handlers.rs` — set on entry creation in the daemon, backward-compatible via `#[serde(default)]`
2. **Add `completed_in_last_hour` + `last_hour_reset_at` to `EmWorkQueueState`** — daemon increments on `remove_terminal_em_work_queue_entry`, resets hourly
3. **Update `queue_stats()`** to compute `throughput` (from `completed_in_last_hour`) and `avg_wait_time_seconds` (from `queued_at` → `assigned_at` delta for assigned entries)
4. **Align the two `EmWorkQueueEntry` structs** — the daemon's struct is missing `held_at` and `hold_reason`; add with `#[serde(default)]` so it round-trips correctly through the shared JSON file
