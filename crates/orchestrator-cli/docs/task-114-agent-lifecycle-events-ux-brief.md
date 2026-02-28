# TASK-114 UX Brief: Agent Lifecycle Events for Pool Visibility

## Phase
- Workflow phase: `ux-research`
- Task: `TASK-114`
- Date: 2026-02-28

---

## 1. User Personas & Goals

### Persona A — Local Operator (primary)
An engineer running `ao daemon run` on a dev machine who wants to know what agents are doing right now. They use `ao daemon events --follow` to watch the live stream and diagnose stalls.

**Goals:**
- "Which tasks are being worked on right now?"
- "How many agents are running? Is the pool full?"
- "How long did the last phase take?"
- "Why did a phase fail?"

### Persona B — Automation / Script Consumer
A script that polls `ao daemon events --json` or tail-reads `daemon-events.jsonl` to feed monitoring dashboards or alert on failures.

**Goals:**
- Machine-parseable event payloads
- Stable event_type strings for filtering
- Pool metrics in every relevant event

### Persona C — Web UI (future)
The embedded Axum web server consuming events via a future SSE/WebSocket endpoint to render a live agent pool panel.

**Goals:**
- Granular lifecycle events to animate pool slots
- `pool_active` / `pool_size` for progress bars
- `duration_secs` for latency histograms

---

## 2. Key User Flows

### Flow 1: Watch live agent pool (terminal)

```
$ ao daemon events --follow
```

Current output (human-readable one line per event):
```
health [/Users/sam/myrepo] 2026-02-28T10:00:00Z
queue [/Users/sam/myrepo] 2026-02-28T10:00:00Z
workflow [/Users/sam/myrepo] 2026-02-28T10:00:00Z
```

**Problem:** No way to see individual agents spawning or completing. The operator sees aggregate counts only after a tick completes (every N seconds), not at the moment an agent starts.

**Desired with TASK-114:**
```
agent-spawned [/Users/sam/myrepo] 2026-02-28T10:00:01Z   task=TASK-007 phase=implementation pool=1/5
agent-spawned [/Users/sam/myrepo] 2026-02-28T10:00:02Z   task=TASK-008 phase=implementation pool=2/5
pool-full [/Users/sam/myrepo] 2026-02-28T10:00:02Z       queued=3 active=5 pool=5
agent-completed [/Users/sam/myrepo] 2026-02-28T10:00:44Z task=TASK-007 phase=implementation dur=43s outcome=advance pool=1/5
pool-backfill [/Users/sam/myrepo] 2026-02-28T10:00:44Z   task=TASK-009 triggered_by=agent-completion
agent-spawned [/Users/sam/myrepo] 2026-02-28T10:00:44Z   task=TASK-009 phase=implementation pool=2/5
agent-failed [/Users/sam/myrepo] 2026-02-28T10:00:55Z    task=TASK-008 phase=implementation error="timeout" pool=1/5
```

### Flow 2: Debug a stalled pool (terminal)

Operator sees no new events for 10 minutes, suspects pool is full:

```
$ ao daemon events --limit 20
```

They scan the last 20 events looking for the last `agent-spawned` and whether a corresponding `agent-completed` or `agent-failed` appeared.

**Key interaction:** Event pairing by `task_id` + `phase_id`. If `agent-spawned` for TASK-007/implementation appears but no completion after 20 minutes, the agent is stuck.

### Flow 3: Script/JSON consumer

```
$ ao daemon events --json --follow | jq 'select(.event_type == "agent-failed")'
```

Machine consumer wants structured data only; human labels in the output don't matter here.

### Flow 4: Future Web UI pool panel

The web UI renders a pool gauge (e.g., "3/5 agents active") updated in real time via SSE. It subscribes to `agent-spawned`, `agent-completed`, `agent-failed` events and increments/decrements the `pool_active` counter on each event.

---

## 3. Key Screens

### Screen 1: `ao daemon events --follow` (human-readable)

**Format (current):**
```
{event_type} [{project_root}] {timestamp}
```

**Format (with new events, proposed enhancement):**
```
{event_type} [{project_root}] {timestamp}  {key=value summary}
```

The one-line `key=value` suffix (appended after the timestamp) makes events scannable without switching to JSON mode. This is a progressive disclosure: the standard three fields for existing events remain unchanged; new agent lifecycle events append a compact summary.

#### Display mapping per event type

| event_type | Key fields to display in summary |
|---|---|
| `agent-spawned` | `task={task_id} phase={phase_id} pool={pool_active}/{pool_size}` |
| `agent-completed` | `task={task_id} phase={phase_id} dur={duration_secs}s outcome={outcome} pool={pool_active}/{pool_size}` |
| `agent-failed` | `task={task_id} phase={phase_id} error="{error}" pool={pool_active}/{pool_size}` |
| `pool-backfill` | `task={task_id} triggered_by={triggered_by}` |
| `pool-full` | `queued={queued_count} active={active_count} pool={pool_size}` |

**Note:** Existing events (health, queue, workflow, etc.) are not changed — they continue printing only the three-part header.

### Screen 2: `ao daemon events --json --follow` (machine-readable)

No change to serialization format. The `data` field of each `DaemonEventRecord` carries the event-specific fields per the schema contracts in the requirements doc.

### Screen 3: Web UI Agent Pool Panel (future, out of scope now but inform design)

Wireframe concept:
```
┌─ Agent Pool ──────────────────────────────────────────┐
│  ██████████░░░░░  3 / 5 active                        │
│                                                        │
│  TASK-007  implementation  running 43s                 │
│  TASK-008  implementation  running 12s  [FAILED]       │
│  TASK-009  implementation  starting...                 │
│                                                        │
│  3 tasks queued                                        │
└────────────────────────────────────────────────────────┘
```

This panel is driven by the event stream. Each `agent-spawned` adds a row; `agent-completed`/`agent-failed` removes and annotates.

---

## 4. Interactions & Ergonomics

### 4.1 Temporal locality

Events must be emitted **at the moment of state change**, not at the end of the tick. This is critical for the `ao daemon events --follow` UX:
- `agent-spawned` must emit when `tokio::spawn` is called for the phase, not at tick summary time.
- `agent-completed`/`agent-failed` must emit when the completion channel is drained.

**Implication for implementation:** These events cannot be batched into `ProjectTickSummary.phase_execution_events` because that structure is emitted at tick end (~1s later). They must be emitted directly via `emit_daemon_event` or use a side-channel signal in the completion path.

### 4.2 Event ordering guarantees

For `--follow` consumers, the expected ordering is:
1. `agent-spawned` before any `agent-completed`/`agent-failed` for the same agent
2. `pool-full` emitted before or simultaneously with the skipped spawn
3. `pool-backfill` emitted after `agent-completed`/`agent-failed` and before the next `agent-spawned`

The JSONL file is append-only; ordering is by write time, which is monotonic within a single daemon process.

### 4.3 Filter ergonomics

Operators filtering events by project are already served by `--project-root`. For filtering by event_type, the current `ao daemon events` command has no `--type` filter — this is a known limitation. The new events should follow the existing `event_type` string convention (kebab-case) to remain consistent with the existing filter tooling.

### 4.4 Pool metrics accuracy

`pool_active` in each event must reflect the count **at the moment of emission**, not a cached value from the previous tick. This means the `in_flight_workflow_ids.len()` from `ReactivePhasePoolState` must be read under the mutex lock at emission time.

---

## 5. Accessibility Constraints (Terminal)

Since this is a CLI tool, accessibility concerns center on:

1. **Color blindness:** The human-readable output uses no color codes. All information is conveyed via text. `outcome=advance` / `outcome=rework` / `outcome=fail` must be self-descriptive strings, not symbols.

2. **Screen reader compatibility:** The one-line format with `key=value` pairs is readable by screen readers. Avoid Unicode box-drawing chars or emoji in the summary field.

3. **Log verbosity / noise floor:** `pool-full` must not be emitted on every tick when the pool is at capacity — this creates log spam. Emit it once per "full" episode (i.e., when the pool transitions to full, not while it remains full).

4. **Line length:** The appended summary should be kept under ~80 chars of additional content to avoid line wrapping on standard 120-char terminals.

---

## 6. Data Schema Review

Reviewing the event payloads from the requirements against UX needs:

| Field | Present in | UX need | Assessment |
|---|---|---|---|
| `task_id` | agent-spawned, agent-completed, agent-failed, pool-backfill | Required for event pairing | ✓ Present |
| `workflow_id` | agent-spawned, agent-completed, agent-failed, pool-backfill | Needed for deep-link to workflow | ✓ Present |
| `phase_id` | agent-spawned, agent-completed, agent-failed | Needed to distinguish retry phases | ✓ Present |
| `pool_active` | agent-spawned, agent-completed, agent-failed | Pool gauge delta | ✓ Present |
| `pool_size` | agent-spawned, agent-completed, agent-failed, pool-full | Pool gauge max | ✓ Present |
| `duration_secs` | agent-completed | Performance tracking | ✓ Present |
| `outcome` | agent-completed | Distinguish advance/rework/fail | ✓ Present |
| `error` | agent-failed | Failure diagnosis | ✓ Present |
| `triggered_by` | pool-backfill | Debug backfill cause | ✓ Present |
| `queued_count` | pool-full | Understand queue pressure | ✓ Present |
| `active_count` | pool-full | Redundant with pool_active but scoped to pool-full | ✓ Present |
| `phase_attempt` | missing | Useful to distinguish retries | ⚠ Not in schema — consider adding |

**Recommendation:** Add `phase_attempt: u32` to `agent-spawned`, `agent-completed`, `agent-failed`. This lets operators distinguish a first attempt from a rework phase execution in the event stream.

---

## 7. Identified Gaps vs. Requirements

| Gap | Severity | Recommendation |
|---|---|---|
| `emit_project_tick_summary_events` emits events at tick-end; lifecycle events need real-time emission | High | Emit directly from spawn/completion path, bypass `ProjectTickSummary` accumulation |
| `pool-full` spam prevention not specified | Medium | Add "last emitted" deduplication — only emit if previous pool-full was >1 tick ago |
| `outcome` values ("advance", "rework", "fail") need mapping from `PhaseExecutionOutcome` enum | Medium | `Completed { phase_decision: None }` → "advance"; `Completed { phase_decision: Some(Rework) }` → "rework"; run_result `Err(_)` → "fail" |
| `pool_size` must come from config but `max_agents` is not accessible where events are emitted | Medium | Pass `pool_size` from `DaemonRunArgs.max_agents` via function parameter into completion path |
| Human-readable event summary line enhancement not in scope of task but needed for usability | Low | Consider adding a separate display ticket, or implement as part of this task |

---

## 8. Summary

The five new event types provide exactly the right level of granularity for pool observability. The main UX concerns are:
1. **Real-time emission** — events must appear in `--follow` mode at the moment of state change, not deferred to tick-end.
2. **Human-readable summary** — appending `key=value` pairs to the existing one-line display format gives operators the signal without switching to JSON.
3. **`pool-full` debouncing** — prevent log spam when pool stays saturated across many ticks.
4. **Schema augmentation** — adding `phase_attempt` improves retry observability at minimal cost.

These are the implementation constraints the `implementation` phase must respect.
