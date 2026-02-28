import React, { useState } from "react";

// ── API Contract Types ────────────────────────────────────────────────────────
// Mirrors EmWorkQueueEntry in queue_handlers.rs

type QueueEntryStatus = "pending" | "assigned" | "held" | "unknown";

interface QueueEntry {
  task_id: string;
  status: QueueEntryStatus;
  workflow_id: string | null;
  queued_at: string | null;
  assigned_at: string | null;
  held_at: string | null;
  hold_reason: string | null;
}

interface QueueListResponse {
  entries: QueueEntry[];
  total: number;
}

interface QueueStatsResponse {
  depth: number;
  pending: number;
  assigned: number;
  held: number;
  ready: number;
  throughput?: number | null;
  avg_wait_time_seconds?: number | null;
}

// ── Fixture Data ──────────────────────────────────────────────────────────────

const SAMPLE_ENTRIES: QueueEntry[] = [
  { task_id: "TASK-041", status: "pending",  workflow_id: null,          queued_at: "2026-02-28T07:00:00Z", assigned_at: null,                   held_at: null,                   hold_reason: null },
  { task_id: "TASK-082", status: "assigned", workflow_id: "wf-run-0441", queued_at: "2026-02-28T07:10:00Z", assigned_at: "2026-02-28T09:14:00Z", held_at: null,                   hold_reason: null },
  { task_id: "TASK-085", status: "assigned", workflow_id: "wf-run-0462", queued_at: "2026-02-28T07:20:00Z", assigned_at: "2026-02-28T09:22:00Z", held_at: null,                   hold_reason: null },
  { task_id: "TASK-091", status: "pending",  workflow_id: null,          queued_at: "2026-02-28T07:30:00Z", assigned_at: null,                   held_at: null,                   hold_reason: null },
  { task_id: "TASK-095", status: "held",     workflow_id: null,          queued_at: "2026-02-28T07:40:00Z", assigned_at: null,                   held_at: "2026-02-28T08:02:00Z", hold_reason: "Blocked externally — waiting for TASK-041." },
  { task_id: "TASK-097", status: "pending",  workflow_id: null,          queued_at: "2026-02-28T07:50:00Z", assigned_at: null,                   held_at: null,                   hold_reason: null },
  { task_id: "TASK-099", status: "held",     workflow_id: null,          queued_at: "2026-02-28T07:30:00Z", assigned_at: null,                   held_at: "2026-02-28T07:45:00Z", hold_reason: "Defer until design review complete." },
  { task_id: "TASK-102", status: "pending",  workflow_id: null,          queued_at: "2026-02-28T08:00:00Z", assigned_at: null,                   held_at: null,                   hold_reason: null },
  { task_id: "TASK-107", status: "pending",  workflow_id: null,          queued_at: "2026-02-28T08:10:00Z", assigned_at: null,                   held_at: null,                   hold_reason: null },
];

const SAMPLE_STATS: QueueStatsResponse = {
  depth: 9, pending: 5, assigned: 2, held: 2, ready: 5,
  throughput: null, avg_wait_time_seconds: null,
};

// ── Sub-Components ────────────────────────────────────────────────────────────

function StatusPill({ status }: { status: QueueEntryStatus }) {
  const classMap: Record<QueueEntryStatus, string> = {
    pending:  "pill pending",
    assigned: "pill assigned",
    held:     "pill held",
    unknown:  "pill",
  };
  return <span className={classMap[status]}>{status}</span>;
}

function StatsPanel({ stats }: { stats: QueueStatsResponse }) {
  const throughputDisplay =
    stats.throughput != null ? `${stats.throughput.toFixed(1)}/hr` : "—";
  const waitDisplay =
    stats.avg_wait_time_seconds != null
      ? `${Math.floor(stats.avg_wait_time_seconds / 60)}m ${stats.avg_wait_time_seconds % 60}s`
      : "—";

  const throughputAbsent = stats.throughput == null;
  const waitAbsent = stats.avg_wait_time_seconds == null;

  return (
    <div className="stats-grid" role="region" aria-label="Queue statistics" aria-live="polite">
      {[
        { label: "Depth",      value: stats.depth,        sub: "total entries",        cls: "" },
        { label: "Pending",    value: stats.pending,      sub: "ready to assign",      cls: "" },
        { label: "Assigned",   value: stats.assigned,     sub: "agent running",        cls: "" },
        { label: "Held",       value: stats.held,         sub: "manually paused",      cls: stats.held > 0 ? " held-color" : "" },
        { label: "Ready",      value: stats.ready,        sub: "= pending",            cls: "" },
        { label: "Throughput", value: throughputDisplay,  sub: "tasks/hr",             cls: throughputAbsent ? " muted" : "" },
        { label: "Avg Wait",   value: waitDisplay,        sub: "time in queue",        cls: waitAbsent ? " muted" : "" },
      ].map(({ label, value, sub, cls }) => (
        <div className="stat-card" key={label}>
          <p className="stat-label">{label}</p>
          <p className={`stat-value${cls}`}>{value}</p>
          <p className="stat-sub">{sub}</p>
        </div>
      ))}
    </div>
  );
}

// ── Queue List View ───────────────────────────────────────────────────────────

function QueueTable({
  entries,
  onHold,
  onRelease,
}: {
  entries: QueueEntry[];
  onHold: (taskId: string) => void;
  onRelease: (taskId: string) => void;
}) {
  return (
    <div className="table-wrap" role="region" aria-label="Work queue entries">
      <table>
        <thead>
          <tr>
            <th>Pos</th>
            <th>Task ID</th>
            <th>Queue status</th>
            <th>Time in queue</th>
            <th>Workflow</th>
            <th>Assigned at</th>
            <th>Held at / reason</th>
            <th>Actions</th>
          </tr>
        </thead>
        <tbody>
          {entries.map((entry, idx) => {
            const timeInQueue = entry.queued_at
              ? (() => {
                  const diffMs = Date.now() - new Date(entry.queued_at).getTime();
                  const mins = Math.floor(diffMs / 60000);
                  const h = Math.floor(mins / 60);
                  const m = mins % 60;
                  return h > 0 ? `${h}h ${m}m` : `${m}m`;
                })()
              : "—";
            return (
            <tr key={entry.task_id}>
              <td><code>{idx + 1}</code></td>
              <td><p className="task-title">{entry.task_id}</p></td>
              <td><StatusPill status={entry.status} /></td>
              <td className="task-meta">{timeInQueue}</td>
              <td className="muted">
                {entry.workflow_id ? <code className="task-meta">{entry.workflow_id}</code> : "—"}
              </td>
              <td className="task-meta">{entry.assigned_at ?? "—"}</td>
              <td>
                {entry.held_at && (
                  <>
                    <p className="task-meta">{entry.held_at}</p>
                    {entry.hold_reason && (
                      <p className="hold-reason-meta">{entry.hold_reason}</p>
                    )}
                  </>
                )}
                {!entry.held_at && <span className="muted">—</span>}
              </td>
              <td>
                <div className="action-stack">
                  {entry.status === "pending" && (
                    <button
                      type="button"
                      className="hold-btn"
                      onClick={() => onHold(entry.task_id)}
                    >
                      Hold
                    </button>
                  )}
                  {entry.status === "held" && (
                    <button
                      type="button"
                      className="success"
                      onClick={() => onRelease(entry.task_id)}
                    >
                      Release
                    </button>
                  )}
                  {entry.status === "assigned" && (
                    <>
                      <button type="button" disabled aria-describedby={`hold-note-${entry.task_id}`}>
                        Hold
                      </button>
                      <p id={`hold-note-${entry.task_id}`} className="rationale">
                        Cannot hold: assigned to running workflow.
                      </p>
                    </>
                  )}
                </div>
              </td>
            </tr>
          );
          })}
        </tbody>
      </table>
    </div>
  );
}

// ── Hold Dialog ───────────────────────────────────────────────────────────────

function HoldDialog({
  taskId,
  onConfirm,
  onCancel,
}: {
  taskId: string;
  onConfirm: (reason: string) => void;
  onCancel: () => void;
}) {
  const [reason, setReason] = useState("");
  return (
    <div
      className="hold-form-card"
      role="dialog"
      aria-modal="true"
      aria-labelledby="hold-dialog-title"
    >
      <h4 id="hold-dialog-title">Place {taskId} on hold</h4>
      <p className="muted">
        <code>POST /api/v1/queue/hold/{taskId}</code>
      </p>
      <label className="field-inline" htmlFor="hold-dialog-reason">
        Hold reason (optional)
        <textarea
          id="hold-dialog-reason"
          rows={3}
          placeholder="e.g. Waiting for upstream decision…"
          value={reason}
          onChange={(e) => setReason(e.target.value)}
        />
      </label>
      <div className="toolbar-row">
        <button type="button" className="hold-btn" onClick={() => onConfirm(reason)}>
          Confirm hold
        </button>
        <button type="button" onClick={onCancel}>
          Cancel
        </button>
      </div>
    </div>
  );
}

// ── Reorder Panel ─────────────────────────────────────────────────────────────

function ReorderPanel({
  entries,
  onSave,
  onDiscard,
}: {
  entries: QueueEntry[];
  onSave: (orderedIds: string[]) => void;
  onDiscard: () => void;
}) {
  const pending = entries.filter((e) => e.status === "pending");
  const held    = entries.filter((e) => e.status === "held");

  const [order, setOrder] = useState(pending.map((e) => e.task_id));
  const [dragging, setDragging] = useState<string | null>(null);
  const [dropTarget, setDropTarget] = useState<string | null>(null);

  function handleDragStart(id: string) {
    setDragging(id);
  }
  function handleDragOver(id: string) {
    if (dragging && id !== dragging) setDropTarget(id);
  }
  function handleDrop(targetId: string) {
    if (!dragging || dragging === targetId) return;
    const next = [...order];
    const from = next.indexOf(dragging);
    const to   = next.indexOf(targetId);
    next.splice(from, 1);
    next.splice(to, 0, dragging);
    setOrder(next);
    setDragging(null);
    setDropTarget(null);
  }

  return (
    <div className="board-columns">
      <article className="panel" aria-labelledby="reorder-title">
        <div className="panel-head">
          <h3 id="reorder-title">Drag to reprioritize</h3>
          <p className="muted">Only pending entries can be moved.</p>
        </div>
        <ul className="drag-list" role="listbox" aria-label="Reorderable queue">
          {order.map((id) => (
            <li
              key={id}
              className={[
                "drag-item",
                dragging === id   ? "dragging"    : "",
                dropTarget === id ? "drop-target" : "",
              ].join(" ").trim()}
              draggable
              onDragStart={() => handleDragStart(id)}
              onDragOver={(e) => { e.preventDefault(); handleDragOver(id); }}
              onDrop={() => handleDrop(id)}
              role="option"
              aria-grabbed={dragging === id}
            >
              <span className="drag-handle" aria-hidden="true">
                <span /><span /><span />
              </span>
              <span className="drag-pos">{order.indexOf(id) + 1}</span>
              <div>
                <p className="drag-task-id">{id}</p>
                <p className="drag-task-meta">pending</p>
              </div>
            </li>
          ))}
          {held.map((e) => (
            <li
              key={e.task_id}
              className="drag-item"
              aria-disabled="true"
              style={{ opacity: 0.6, cursor: "not-allowed" }}
            >
              <span className="drag-pos">—</span>
              <div>
                <p className="drag-task-id">
                  {e.task_id} <StatusPill status="held" />
                </p>
                <p className="drag-task-meta">locked — held entries excluded</p>
              </div>
            </li>
          ))}
        </ul>
        <div className="toolbar-row" style={{ marginTop: 12 }}>
          <button type="button" className="success" onClick={() => onSave(order)}>
            Save order
          </button>
          <button type="button" onClick={onDiscard}>Discard</button>
        </div>
      </article>

      <article className="panel" aria-labelledby="diff-title">
        <div className="panel-head">
          <h3 id="diff-title">Order diff</h3>
          <p className="muted">Pending items only</p>
        </div>
        <div className="reorder-diff">
          <div className="diff-col">
            <p className="diff-col-label">Before</p>
            {pending.map((e, i) => (
              <div key={e.task_id} className="diff-row">
                <span>{i + 1}</span>
                <span>{e.task_id}</span>
              </div>
            ))}
          </div>
          <div className="diff-col">
            <p className="diff-col-label">After</p>
            {order.map((id, i) => {
              const moved = pending.findIndex((e) => e.task_id === id) !== i;
              return (
                <div key={id} className={`diff-row${moved ? " moved" : ""}`}>
                  {moved && <span className="diff-arrow">↑</span>}
                  <span>{i + 1}</span>
                  <span>{id}</span>
                </div>
              );
            })}
          </div>
        </div>
        <div className="status-card" style={{ marginTop: 12 }}>
          <dl>
            <div>
              <dt>Payload</dt>
              <dd>
                <code>{"{ \"task_ids\": [" + order.map((id) => `"${id}"`).join(", ") + "] }"}</code>
              </dd>
            </div>
          </dl>
        </div>
      </article>
    </div>
  );
}

// ── Root Component ─────────────────────────────────────────────────────────────

type View = "list" | "reorder";

export default function QueueManagementWireframe() {
  const [entries, setEntries] = useState<QueueEntry[]>(SAMPLE_ENTRIES);
  const [view, setView] = useState<View>("list");
  const [holdTarget, setHoldTarget] = useState<string | null>(null);

  function handleHold(taskId: string) {
    setHoldTarget(taskId);
  }

  function confirmHold(taskId: string, reason: string) {
    setEntries((prev) =>
      prev.map((e) =>
        e.task_id === taskId
          ? { ...e, status: "held" as const, held_at: new Date().toISOString(), hold_reason: reason || null }
          : e
      )
    );
    setHoldTarget(null);
  }

  function confirmRelease(taskId: string) {
    setEntries((prev) =>
      prev.map((e) =>
        e.task_id === taskId
          ? { ...e, status: "pending" as const, held_at: null, hold_reason: null }
          : e
      )
    );
  }

  function saveReorder(orderedIds: string[]) {
    const ordered: QueueEntry[] = [];
    for (const id of orderedIds) {
      const entry = entries.find((e) => e.task_id === id);
      if (entry) ordered.push(entry);
    }
    for (const entry of entries) {
      if (!orderedIds.includes(entry.task_id)) ordered.push(entry);
    }
    setEntries(ordered);
    setView("list");
  }

  const stats: QueueStatsResponse = {
    depth:    entries.length,
    pending:  entries.filter((e) => e.status === "pending").length,
    assigned: entries.filter((e) => e.status === "assigned").length,
    held:     entries.filter((e) => e.status === "held").length,
    ready:    entries.filter((e) => e.status === "pending").length,
  };

  return (
    <div>
      <header className="doc-header">
        <h1>TASK-085: Queue Management</h1>
        <p>Interactive wireframe — all state is local; no real API calls are made.</p>
      </header>

      <main className="board-grid">
        <section className="board" aria-labelledby="stats-section">
          <h2 className="board-title" id="stats-section">
            Queue Dashboard (<code>GET /api/v1/queue/stats</code> + <code>GET /api/v1/queue</code>)
          </h2>
          <div className="control-shell">
            <StatsPanel stats={stats} />
            <div className="toolbar-row">
              <button
                type="button"
                onClick={() => setView("list")}
                aria-pressed={view === "list"}
              >
                Queue list
              </button>
              <button
                type="button"
                onClick={() => setView("reorder")}
                aria-pressed={view === "reorder"}
              >
                Reorder mode
              </button>
            </div>

            {view === "list" && (
              <QueueTable
                entries={entries}
                onHold={handleHold}
                onRelease={confirmRelease}
              />
            )}

            {view === "reorder" && (
              <ReorderPanel
                entries={entries}
                onSave={saveReorder}
                onDiscard={() => setView("list")}
              />
            )}

            {holdTarget && (
              <div role="dialog" aria-modal="true" style={{ marginTop: 16 }}>
                <HoldDialog
                  taskId={holdTarget}
                  onConfirm={(reason) => confirmHold(holdTarget, reason)}
                  onCancel={() => setHoldTarget(null)}
                />
              </div>
            )}
          </div>
        </section>
      </main>
    </div>
  );
}
