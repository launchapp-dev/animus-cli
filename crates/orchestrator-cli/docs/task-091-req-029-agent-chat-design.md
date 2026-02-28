# TASK-091: REQ-029 Agent-First Web App — Design Specification

## Phase
- Workflow phase: `implementation`
- Task: `TASK-091`
- Requirement: `REQ-029`

## Objective

Refine and ground REQ-029 against the real API surface. Provide testable acceptance
criteria, a gap analysis of the 6-page structure, the Agent Chat interaction model,
data contracts for SSE-based agent output streaming, and a phased MVP definition.

---

## Current API Capability Audit

### Available REST endpoints relevant to the 6-page structure

| Page | Required capability | Existing endpoint | Status |
|---|---|---|---|
| Agent Chat | List active agent sessions | `GET /api/v1/daemon/agents` | Exists — verify response fields |
| Agent Chat | Stream agent output | `GET /api/v1/events` (SSE, `AgentOutputChunk` events) | Exists |
| Agent Chat | Deep-link to session | `/agents/:runId` frontend route | Missing |
| Agent Chat | Send message to agent | None | GAP — MVP deferred |
| Agent Chat | Session history replay | `GET /api/v1/output/run` (via MCP; no direct REST) | Partial |
| Task Board | List tasks by status | `GET /api/v1/tasks?status=...` | Exists |
| Task Board | Update task status | `POST /api/v1/tasks/:id/status` | Exists |
| Task Board | Slide-out detail | Client-side routing | Frontend-only |
| Workflow Monitor | List active workflows | `GET /api/v1/workflows` | Exists |
| Workflow Monitor | Phase decisions | `GET /api/v1/workflows/:id/decisions` | Exists |
| Workflow Monitor | Phase pipeline state | `current_phase`, `current_phase_index` in workflow response | Partial |
| Planning Hub | Vision editor | `GET/POST /api/v1/vision`, `POST /api/v1/vision/refine` | Exists |
| Planning Hub | Requirements list | `GET /api/v1/requirements` | Exists |
| Planning Hub | Task generation | `POST /api/v1/requirements/draft` | Exists |
| Dashboard | Task counts | `GET /api/v1/tasks/stats` | Exists |
| Dashboard | Daemon health | `GET /api/v1/daemon/health`, `GET /api/v1/daemon/status` | Exists |
| Dashboard | Throughput/error rate | None | GAP |
| Dashboard | Recent decisions summary | None | GAP |
| Settings | Read agent runtime config | None | GAP |
| Settings | Write daemon config | None | GAP |

### Gaps requiring new API endpoints (beyond MVP)

- `GET /api/v1/metrics/throughput` — tasks completed per hour/day
- `GET /api/v1/metrics/errors` — recent error events with counts
- `GET /api/v1/metrics/decisions` — last N cross-workflow phase decisions
- `GET /api/v1/settings/agent-runtime-config` — read current config
- `PATCH /api/v1/settings/agent-runtime-config` — write config (high risk, out of scope)
- WebSocket endpoint for bidirectional agent messaging (full vision)

---

## Agent Chat Page Interaction Model

### Architecture

Agent Chat is a **read-only observation interface** in MVP. It consumes two data
sources:

1. **Session list**: `GET /api/v1/daemon/agents` — polled every 10s or refreshed
   on `AgentStatusChanged` SSE events.
2. **Output stream**: `GET /api/v1/events` — persistent SSE connection filtering
   `AgentOutputChunk` events by `agent_id` for the selected session.

### Session Selection Flow

```
Page load
  → GET /daemon/agents
  → Render session list (left panel)
  → If URL is /agents/:runId, auto-select that session
  → Else, auto-select first active session (or show empty state)

Session selected
  → Set active agent_id
  → Filter incoming SSE events: event.data.event_type === "agent_output_chunk"
                                  && event.data.agent_id === activeAgentId
  → Render output chunks in chat view (main panel)
  → Fetch task context: GET /tasks/:taskId (from session metadata)
  → Fetch workflow: GET /workflows (filter by task_id)
```

### SSE Event Filtering for Agent Output

The SSE endpoint broadcasts all daemon events. Agent Chat filters client-side:

```typescript
// Filter predicate for agent output chunks
function isAgentOutputChunk(event: DaemonEventRecord): boolean {
  return event.event_type === "agent_output_chunk";
}

function isForActiveSession(event: DaemonEventRecord, agentId: string): boolean {
  return (event.data as { agent_id?: string }).agent_id === agentId;
}
```

### Output Rendering Rules

| Stream type / content pattern | Render as |
|---|---|
| `stream_type: "stdout"`, plain text | Chat message bubble |
| `stream_type: "stdout"`, starts with `{"kind":` | JSON decision card (collapsible) |
| `stream_type: "stdout"`, fenced code block | Syntax-highlighted code block |
| `stream_type: "stderr"` | Error banner (red) |
| `event_type: "thinking"` | Collapsible reasoning block (distinct background) |
| `event_type: "tool_call"` | Tool invocation card (tool name + params) |
| `event_type: "tool_result"` | Tool result card (success/failure indicator) |
| `event_type: "artifact"` | File artifact preview (path + size) |

### Reconnection Behavior

The existing `useDaemonEvents` hook implements SSE reconnection with:
- Exponential backoff: 1s → 10s max
- `Last-Event-ID` header on reconnect for sequence-based resumption
- 200-event in-memory buffer

Agent Chat must not replay historical output after reconnect (sequence tracking
handles deduplication at the SSE layer). The buffer stores recent events across
all agents; chat filtering happens in the component.

### Session Persistence

- Agent output is persisted in `<run_dir>/events.jsonl` on the server
- When a session ends, the frontend shows "Completed" status with final verdict
- Output from completed sessions can be reviewed via the decision history in the
  context sidebar (fetched from `GET /api/v1/workflows/:id/decisions`)

### Bidirectional Messaging (Full Vision — Not MVP)

Full agent messaging requires:
1. A WebSocket endpoint at `WS /api/v1/agents/:runId/chat`
2. Server-side injection of user messages into the agent runner IPC channel
3. New `AgentChatMessage` protocol type in `crates/protocol`
4. A message queue in the runner to process injected prompts

This is deferred to a post-MVP phase because it requires changes to the agent-runner
IPC protocol and session lifecycle.

---

## SSE Data Contracts for Agent Chat

### DaemonEventRecord (existing — from crates/orchestrator-web-contracts/src/lib.rs)

```typescript
interface DaemonEventRecord {
  schema: string;       // e.g. "ao.daemon.v1.event"
  id: string;           // UUID
  seq: number;          // Monotonic sequence number
  timestamp: string;    // RFC3339
  event_type: string;   // Discriminator (see below)
  project_root: string | null;
  data: unknown;        // Event-specific payload
}
```

### Agent Output Chunk Event

```typescript
// event_type: "agent_output_chunk"
interface AgentOutputChunkData {
  agent_id: string;
  stream_type: "stdout" | "stderr" | "system";
  text: string;
}
```

### Agent Status Changed Event

```typescript
// event_type: "agent_status_changed"
interface AgentStatusChangedData {
  agent_id: string;
  status: "starting" | "running" | "paused" | "completed" | "failed" | "timeout" | "terminated";
  elapsed_ms: number;
}
```

### Workflow Phase Changed Event

```typescript
// event_type: "workflow_phase_changed"
interface WorkflowPhaseChangedData {
  project_id: string;
  phase: string;         // Phase name (e.g. "implementation")
  status: "pending" | "running" | "passed" | "failed" | "rework";
}
```

### Stats Update Event

```typescript
// event_type: "stats_update"
interface StatsUpdateData {
  project_id: string;
  stats: {
    total: number;
    in_progress: number;
    blocked: number;
    completed: number;
    by_status: Record<string, number>;
  };
}
```

### Daemon Agents Response (GET /api/v1/daemon/agents)

```typescript
// Required fields for Agent Chat session list
interface AgentSession {
  run_id: string;
  agent_id: string;
  task_id: string | null;
  phase: string | null;
  model: string;
  tool: string;
  status: "starting" | "running" | "paused" | "completed" | "failed";
  elapsed_ms: number;
  started_at: string; // RFC3339
}
```

---

## MVP vs Full Vision Phasing

### MVP (Phase 1) — Implement with existing API surface

**In scope:**

| Feature | Implementation approach |
|---|---|
| Agent Chat page — session list | Poll `GET /daemon/agents` every 10s + SSE `agent_status_changed` |
| Agent Chat page — output stream | Filter SSE `agent_output_chunk` by `agent_id` |
| Agent Chat page — deep-link URL | Frontend route `/agents/:runId` |
| Agent Chat page — context sidebar | Fetch task + workflow via existing REST endpoints |
| Task Board — kanban columns | Visual upgrade; use existing `GET/POST /tasks` API |
| Task Board — status transitions | `POST /tasks/:id/status` on column drop or button |
| Task Board — slide-out detail | Client-side state (no navigation); existing task detail API |
| Workflow Monitor — phase pipeline | Render phases from `current_phase` + decisions list |
| Workflow Monitor — drill-down | Link phase node to `/agents` filtered by run |
| Planning Hub | Already largely implemented; minor UI refinements |
| Dashboard — task counts | Use `GET /tasks/stats` + SSE `stats_update` |
| Dashboard — daemon health | Use `GET /daemon/health` |
| Navigation — agent badge | Count from `GET /daemon/agents`, refresh on `agent_status_changed` |

**Not in scope for MVP:**

- Bidirectional agent messaging (no WebSocket)
- Dashboard throughput and error rate metrics (no backend endpoint)
- Settings page (no config API)
- Agent output history replay for completed sessions in Agent Chat

### Full Vision (Phase 2) — Requires new API surface

- WebSocket bidirectional channel: `WS /api/v1/agents/:runId/chat`
- Throughput metrics endpoint: `GET /api/v1/metrics/throughput`
- Error rate endpoint: `GET /api/v1/metrics/errors`
- Settings read/write API
- Drag-and-drop with server-side dependency validation
- Agent output history retrieval for completed sessions

---

## Testable Acceptance Criteria

### AC-01: Agent session list
`GET /api/v1/daemon/agents` returns an array. Each item includes `run_id`,
`agent_id`, `task_id`, `phase`, `model`, `status`, and `elapsed_ms`. Frontend
renders one card per session in the left panel of `/agents`.

### AC-02: Agent output streaming
SSE events with `event_type === "agent_output_chunk"` are rendered in the chat
view for the session matching `data.agent_id`. Switching sessions clears the
view and shows output for the new session within one SSE keepalive cycle (≤15s).

### AC-03: Agent Chat deep-link
Navigating to `/agents/:runId` directly renders that session's output if
`runId` matches an active or recently completed run. Unknown `runId` shows
a "Session not found" message within the agents layout (no full-page error).

### AC-04: Output type rendering
Given a stream of `AgentOutputChunk` events with mixed `text` content,
the chat view renders: plain text as chat bubbles, JSON starting with `{"kind":`
as collapsible decision cards, code fences as syntax-highlighted blocks,
and `stream_type: "stderr"` text with an error indicator.

### AC-05: SSE reconnection
When the SSE connection drops and reconnects, output does not restart from the
beginning. The `Last-Event-ID` header is sent on reconnect with the last
received sequence number. No visible duplication in the chat view.

### AC-06: Task Board kanban columns
`GET /api/v1/tasks` data is rendered in five columns: Backlog, Ready, In Progress,
Blocked, Done. Column assignment matches task `status` field.

### AC-07: Task status transition
Dragging a task card to a different column (or using the column action button)
calls `POST /api/v1/tasks/:id/status` with the new status. On API success,
the card appears in the new column without page reload. On API failure, the
card reverts to its original column and an error message is shown.

### AC-08: Workflow phase pipeline
`GET /api/v1/workflows` data renders each active workflow as a horizontal phase
sequence. Phases are derived from the pipeline definition (requirements →
research → implementation → code-review → testing). The current phase node
is visually distinguished (highlighted/active indicator).

### AC-09: Phase decision drill-down
Clicking a completed phase node in Workflow Monitor fetches
`GET /api/v1/workflows/:id/decisions` and displays the matching decision:
`verdict` (advance/rework/fail), `confidence` (0.0–1.0), `risk`, and `reason`.

### AC-10: Dashboard task counts
Dashboard shows total, in-progress, blocked, and done task counts. Counts
refresh without page reload when an SSE `stats_update` event is received.

### AC-11: Daemon control
Dashboard shows daemon status from `GET /api/v1/daemon/health`. Start and Stop
buttons call `POST /daemon/start` and `POST /daemon/stop` respectively. Status
indicator updates within one SSE keepalive cycle after the action.

### AC-12: Navigation agent badge
The navigation bar shows a count of active agents (from `GET /daemon/agents`).
The count updates when an `agent_status_changed` SSE event is received.

### AC-13: Responsive layout
All six pages render without layout overflow or element clipping on viewports
≥768px wide (tablet). Touch targets for interactive elements are ≥44×44px.

### AC-14: Deep-link route integrity
All routes (`/agents`, `/agents/:runId`, `/tasks`, `/workflows`, `/workflows/:id`,
`/planning`, `/planning/vision`, `/planning/requirements`, `/dashboard`,
`/settings`) render without a full-page error when navigated to directly
(browser hard refresh). Unknown routes render the existing "Not Found" page.

---

## Implementation Notes

### Primary source targets for MVP

- `crates/orchestrator-web-server/web-ui/src/app/router.tsx` — add `/agents` and
  `/agents/:runId` routes
- `crates/orchestrator-web-server/web-ui/src/app/screens.tsx` — add Agent Chat page
  component and Task Board kanban layout
- `crates/orchestrator-web-server/web-ui/src/lib/events/use-daemon-events.ts` —
  existing hook; no changes needed, just consume from Agent Chat
- `crates/orchestrator-web-server/web-ui/src/lib/api/contracts/models.ts` — add
  `AgentSession` type and event data interfaces
- `crates/orchestrator-web-api/src/services/web_api_service/` — verify `daemon/agents`
  response includes `phase` and `task_id` fields; add if missing

### Out of scope for TASK-091

- Implementing the Agent Chat page component (tracked by TASK-065)
- Implementing the Task Board kanban (tracked by TASK-066)
- Implementing the Workflow Monitor pipeline view (tracked by TASK-067)
- Implementing the WebSocket bidirectional channel
