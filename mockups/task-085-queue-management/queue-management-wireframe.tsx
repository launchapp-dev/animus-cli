/**
 * TASK-085 wireframe scaffold.
 * Intent: React-oriented contracts for queue management UI — ordered work items,
 * hold/release controls, drag-to-reorder, stats panel, and empty/loading states.
 *
 * Endpoints backed by this UI:
 *   GET  /api/queue              — list queue with enriched task data
 *   GET  /api/queue/stats        — queue depth, throughput, avg wait time
 *   POST /api/queue/reorder      — body: { task_ids: string[] }
 *   POST /api/queue/hold/:id     — body: { reason?: string }
 *   POST /api/queue/release/:id  — body: { reason?: string }
 */

import { useCallback, useId, useMemo, useRef, useState } from "react";

type QueueEntryStatus = "pending" | "assigned" | "held";
type TaskPriority = "critical" | "high" | "medium" | "low";
type TaskStatus =
  | "backlog"
  | "ready"
  | "in-progress"
  | "blocked"
  | "on-hold"
  | "done"
  | "cancelled";

type QueueEntryTask = {
  id: string;
  title: string;
  description: string;
  status: TaskStatus;
  priority: TaskPriority;
};

type QueueEntry = {
  task_id: string;
  status: QueueEntryStatus;
  workflow_id: string | null;
  assigned_at: string | null;
  held_at: string | null;
  task: QueueEntryTask | null;
};

type QueueListStats = {
  total: number;
  pending: number;
  assigned: number;
  held: number;
};

type QueueListResponse = {
  entries: QueueEntry[];
  stats: QueueListStats;
};

type QueueStatsResponse = {
  depth: number;
  pending: number;
  assigned: number;
  held: number;
  throughput_last_hour: number;
  avg_wait_time_secs: number;
};

type QueueReorderRequest = {
  task_ids: string[];
};

type QueueHoldRequest = {
  reason?: string;
};

type QueueReleaseRequest = {
  reason?: string;
};

type QueueHoldResponse = {
  held: boolean;
  task_id: string;
};

type QueueReleaseResponse = {
  released: boolean;
  task_id: string;
};

type QueueReorderResponse = {
  reordered: boolean;
};

type HoldActionState =
  | { kind: "idle" }
  | { kind: "confirming"; taskId: string; reason: string }
  | { kind: "submitting"; taskId: string }
  | { kind: "success"; taskId: string }
  | { kind: "error"; taskId: string; message: string };

type ReleaseActionState =
  | { kind: "idle" }
  | { kind: "confirming"; taskId: string }
  | { kind: "submitting"; taskId: string }
  | { kind: "success"; taskId: string }
  | { kind: "error"; taskId: string; message: string };

type ReorderState =
  | { kind: "idle" }
  | { kind: "dragging"; draggedId: string; overId: string | null }
  | { kind: "submitting"; orderedIds: string[] }
  | { kind: "success" }
  | { kind: "error"; message: string };

type QueueLoadState =
  | { kind: "loading" }
  | { kind: "error"; message: string }
  | { kind: "loaded"; data: QueueListResponse };

type StatsLoadState =
  | { kind: "loading" }
  | { kind: "error"; message: string }
  | { kind: "loaded"; data: QueueStatsResponse };

function formatWaitTime(secs: number): string {
  if (secs <= 0) return "—";
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m`;
  return `${Math.floor(secs / 3600)}h ${Math.floor((secs % 3600) / 60)}m`;
}

function formatTimestamp(iso: string | null): string {
  if (!iso) return "—";
  const parsed = Date.parse(iso);
  if (Number.isNaN(parsed)) return iso;
  return `${new Date(parsed).toISOString().slice(0, 19)} UTC`;
}

const priorityOrder: Record<TaskPriority, number> = {
  critical: 0,
  high: 1,
  medium: 2,
  low: 3,
};

function QueueEntryRow({
  entry,
  index,
  total,
  holdState,
  releaseState,
  reorderState,
  onHoldRequest,
  onReleaseRequest,
  onDragStart,
  onDragOver,
  onDrop,
}: {
  entry: QueueEntry;
  index: number;
  total: number;
  holdState: HoldActionState;
  releaseState: ReleaseActionState;
  reorderState: ReorderState;
  onHoldRequest: (taskId: string) => void;
  onReleaseRequest: (taskId: string) => void;
  onDragStart: (taskId: string) => void;
  onDragOver: (taskId: string) => void;
  onDrop: () => void;
}) {
  const isDragging =
    reorderState.kind === "dragging" && reorderState.draggedId === entry.task_id;
  const isDragOver =
    reorderState.kind === "dragging" && reorderState.overId === entry.task_id;

  const isHolding =
    holdState.kind === "submitting" && holdState.taskId === entry.task_id;
  const isReleasing =
    releaseState.kind === "submitting" && releaseState.taskId === entry.task_id;
  const isHeld = entry.status === "held";

  return (
    <li
      draggable
      aria-label={`Queue entry ${index + 1} of ${total}: ${entry.task?.title ?? entry.task_id}`}
      data-dragging={isDragging}
      data-dragover={isDragOver}
      onDragStart={() => onDragStart(entry.task_id)}
      onDragOver={(e) => { e.preventDefault(); onDragOver(entry.task_id); }}
      onDrop={onDrop}
    >
      <span aria-hidden="true" className="drag-handle">⠿</span>
      <span className="queue-position" aria-label={`Position ${index + 1}`}>
        {index + 1}
      </span>

      <div className="entry-body">
        <div className="entry-title-row">
          <span className="entry-task-id">{entry.task_id}</span>
          <span className={`priority-chip priority-${entry.task?.priority ?? "medium"}`}>
            {entry.task?.priority ?? "—"}
          </span>
          <span className={`status-chip status-${entry.status}`}>{entry.status}</span>
        </div>
        <p className="entry-title">{entry.task?.title ?? "(task not found)"}</p>
        {entry.held_at && (
          <p className="entry-meta">Held at: {formatTimestamp(entry.held_at)}</p>
        )}
        {entry.assigned_at && (
          <p className="entry-meta">Assigned at: {formatTimestamp(entry.assigned_at)}</p>
        )}
      </div>

      <div className="entry-actions">
        {isHeld ? (
          <button
            type="button"
            onClick={() => onReleaseRequest(entry.task_id)}
            disabled={isReleasing}
            aria-busy={isReleasing}
            aria-label={`Release ${entry.task_id} from hold`}
          >
            {isReleasing ? "Releasing…" : "Release"}
          </button>
        ) : (
          <button
            type="button"
            className="hold-btn"
            onClick={() => onHoldRequest(entry.task_id)}
            disabled={isHolding || entry.status === "assigned"}
            aria-disabled={entry.status === "assigned"}
            aria-label={
              entry.status === "assigned"
                ? `${entry.task_id} is assigned — cannot hold`
                : `Hold ${entry.task_id}`
            }
          >
            {isHolding ? "Holding…" : "Hold"}
          </button>
        )}
      </div>
    </li>
  );
}

function QueueStatsPanel({ stats }: { stats: QueueStatsResponse }) {
  return (
    <section aria-labelledby="queue-stats-title" className="stats-panel">
      <h3 id="queue-stats-title">Queue Stats</h3>
      <dl className="stats-grid">
        <div className="stat-item">
          <dt>Queue depth</dt>
          <dd>{stats.depth}</dd>
        </div>
        <div className="stat-item">
          <dt>Pending</dt>
          <dd>{stats.pending}</dd>
        </div>
        <div className="stat-item">
          <dt>Assigned</dt>
          <dd>{stats.assigned}</dd>
        </div>
        <div className="stat-item stat-item--held">
          <dt>On hold</dt>
          <dd>{stats.held}</dd>
        </div>
        <div className="stat-item">
          <dt>Throughput (last hour)</dt>
          <dd>{stats.throughput_last_hour} tasks</dd>
        </div>
        <div className="stat-item">
          <dt>Avg wait time</dt>
          <dd>{formatWaitTime(stats.avg_wait_time_secs)}</dd>
        </div>
      </dl>
    </section>
  );
}

function HoldConfirmDialog({
  state,
  onConfirm,
  onCancel,
  onReasonChange,
}: {
  state: Extract<HoldActionState, { kind: "confirming" }>;
  onConfirm: () => void;
  onCancel: () => void;
  onReasonChange: (reason: string) => void;
}) {
  const reasonId = useId();
  return (
    <div role="dialog" aria-modal="true" aria-labelledby="hold-dialog-title" className="confirm-dialog">
      <h4 id="hold-dialog-title">Hold {state.taskId}?</h4>
      <p>This task will be paused in the work queue and skipped by the scheduler.</p>
      <label htmlFor={reasonId}>Reason (optional)</label>
      <input
        id={reasonId}
        type="text"
        value={state.reason}
        onChange={(e) => onReasonChange(e.target.value)}
        placeholder="e.g. waiting for dependency"
        aria-label="Hold reason"
      />
      <div className="dialog-actions">
        <button type="button" className="hold-btn" onClick={onConfirm}>
          Confirm Hold
        </button>
        <button type="button" className="ghost-btn" onClick={onCancel}>
          Cancel
        </button>
      </div>
    </div>
  );
}

export function QueueDashboard() {
  const [queueState, setQueueState] = useState<QueueLoadState>({ kind: "loading" });
  const [statsState, setStatsState] = useState<StatsLoadState>({ kind: "loading" });
  const [holdState, setHoldState] = useState<HoldActionState>({ kind: "idle" });
  const [releaseState, setReleaseState] = useState<ReleaseActionState>({ kind: "idle" });
  const [reorderState, setReorderState] = useState<ReorderState>({ kind: "idle" });
  const liveRegionRef = useRef<HTMLDivElement>(null);

  const announceToLiveRegion = useCallback((message: string) => {
    if (liveRegionRef.current) {
      liveRegionRef.current.textContent = message;
    }
  }, []);

  const entries = queueState.kind === "loaded" ? queueState.data.entries : [];

  const reorderedEntries = useMemo(() => {
    if (reorderState.kind !== "dragging" || !reorderState.overId || !reorderState.draggedId) {
      return entries;
    }
    const result = entries.filter((e) => e.task_id !== reorderState.draggedId);
    const overIdx = result.findIndex((e) => e.task_id === reorderState.overId);
    const dragged = entries.find((e) => e.task_id === reorderState.draggedId);
    if (dragged && overIdx >= 0) {
      result.splice(overIdx, 0, dragged);
    }
    return result;
  }, [entries, reorderState]);

  return (
    <main className="queue-dashboard" aria-label="Work Queue Management">
      <header className="queue-header">
        <h2>Work Queue</h2>
        <p className="breadcrumbs">Queue / Management</p>
        <div className="state-strip" aria-label="Queue states shown">
          <span className="state-chip">pending</span>
          <span className="state-chip pending">held</span>
          <span className="state-chip success">assigned</span>
          <span className="state-chip">empty</span>
        </div>
      </header>

      <div ref={liveRegionRef} aria-live="polite" aria-atomic="true" className="sr-only" />

      {holdState.kind === "confirming" && (
        <HoldConfirmDialog
          state={holdState}
          onConfirm={() => { /* submit hold */ }}
          onCancel={() => setHoldState({ kind: "idle" })}
          onReasonChange={(reason) =>
            setHoldState((s) => s.kind === "confirming" ? { ...s, reason } : s)
          }
        />
      )}

      <div className="queue-layout">
        <section aria-labelledby="queue-list-title" className="queue-list-section">
          <div className="queue-list-header">
            <h3 id="queue-list-title">
              Queue{" "}
              {queueState.kind === "loaded" && (
                <span className="count-badge">{queueState.data.stats.total}</span>
              )}
            </h3>
            {reorderState.kind === "dragging" && (
              <span className="reorder-hint" aria-live="polite">
                Drag to reorder — release to drop
              </span>
            )}
            {reorderState.kind === "submitting" && (
              <span className="reorder-hint" aria-live="polite">Saving order…</span>
            )}
            {reorderState.kind === "error" && (
              <span className="inline-error-inline" role="alert">
                Reorder failed: {reorderState.message}
              </span>
            )}
          </div>

          {queueState.kind === "loading" && (
            <p className="loading-state" aria-busy="true" aria-label="Loading queue">
              Loading queue…
            </p>
          )}

          {queueState.kind === "error" && (
            <div className="inline-error" role="alert">
              <strong>queue_load_failed</strong>
              <p>{queueState.message}</p>
            </div>
          )}

          {queueState.kind === "loaded" && entries.length === 0 && (
            <div className="empty-state" role="status">
              <h4>Queue is empty</h4>
              <p>
                No tasks are currently queued. Tasks in <code>ready</code> status will appear
                here once the scheduler picks them up.
              </p>
            </div>
          )}

          {queueState.kind === "loaded" && entries.length > 0 && (
            <ol className="queue-entry-list" aria-label="Queued tasks in priority order">
              {reorderedEntries.map((entry, idx) => (
                <QueueEntryRow
                  key={entry.task_id}
                  entry={entry}
                  index={idx}
                  total={reorderedEntries.length}
                  holdState={holdState}
                  releaseState={releaseState}
                  reorderState={reorderState}
                  onHoldRequest={(id) =>
                    setHoldState({ kind: "confirming", taskId: id, reason: "" })
                  }
                  onReleaseRequest={(id) =>
                    setReleaseState({ kind: "confirming", taskId: id })
                  }
                  onDragStart={(id) =>
                    setReorderState({ kind: "dragging", draggedId: id, overId: null })
                  }
                  onDragOver={(id) =>
                    setReorderState((s) =>
                      s.kind === "dragging" ? { ...s, overId: id } : s
                    )
                  }
                  onDrop={() => {
                    if (reorderState.kind === "dragging") {
                      setReorderState({ kind: "submitting", orderedIds: reorderedEntries.map((e) => e.task_id) });
                      announceToLiveRegion("Queue reorder submitted.");
                    }
                  }}
                />
              ))}
            </ol>
          )}
        </section>

        <aside className="queue-stats-aside" aria-label="Queue statistics">
          {statsState.kind === "loading" && (
            <p className="loading-state" aria-busy="true">Loading stats…</p>
          )}
          {statsState.kind === "error" && (
            <div className="inline-error" role="alert">
              <strong>stats_load_failed</strong>
              <p>{statsState.message}</p>
            </div>
          )}
          {statsState.kind === "loaded" && (
            <QueueStatsPanel stats={statsState.data} />
          )}
        </aside>
      </div>
    </main>
  );
}
