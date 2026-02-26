/**
 * TASK-014 wireframe scaffold.
 * Intent: React-oriented contracts for deterministic queue operations,
 * workflow controls, ordered timeline rendering, and fail-closed gating UX.
 */

import { useMemo, useRef, useState } from "react";

type TaskPriority = "critical" | "high" | "medium" | "low";
type CanonicalTaskStatus =
  | "backlog"
  | "ready"
  | "in-progress"
  | "blocked"
  | "on-hold"
  | "done"
  | "cancelled";
type TaskStatusToken = CanonicalTaskStatus | "todo" | "in_progress" | "on_hold";
type WorkflowStatus = "idle" | "running" | "paused" | "cancelled" | "completed";
type WorkflowAction = "workflow.run" | "workflow.resume" | "workflow.pause" | "workflow.cancel";
type GateState = "approved" | "pending" | "rejected";
type CheckpointState = "open" | "pending" | "approved" | "rejected";
type TimelineState = "checkpoint" | "decision" | "blocked" | "approved";

type ActionAvailability = {
  enabled: boolean;
  reason: string | null;
  requiresConfirmation: boolean;
};

type TaskChecklist = {
  done: number;
  total: number;
};

type TaskQueueItem = {
  id: string;
  title: string;
  priority: TaskPriority;
  status: TaskStatusToken;
  assignee: string;
  dependencyIds: string[];
  checklist: TaskChecklist;
  updatedAt: string;
};

type PhaseTimelineEntry = {
  id: string;
  checkpointOrder: number;
  checkpointKey: string;
  phase: "planning" | "design" | "wireframe" | "implementation" | "qa";
  state: TimelineState;
  timestamp: string;
  checkpointState?: CheckpointState;
  decision?: string;
  approver?: string;
  blockerReason?: string;
};

type TelemetryOutcome = "success" | "conflict" | "idempotent-retry";

type TelemetryEntry = {
  id: string;
  timestamp: string;
  action: string;
  endpoint: string;
  actor: string;
  correlationId: string;
  outcome: TelemetryOutcome;
  message: string;
};

type ConfirmationGate =
  | { kind: "workflow.cancel"; targetId: string; phrase: string }
  | { kind: "task.transition.cancelled"; targetId: string; phrase: string };

type QueueState = "ready" | "empty" | "filtered-empty";

type AoSuccessEnvelope<TData> = {
  schema: "ao.cli.v1";
  ok: true;
  data: TData;
};

type AoErrorEnvelope = {
  schema: "ao.cli.v1";
  ok: false;
  error: {
    code: string;
    message: string;
    exit_code: number;
  };
};

type ApiResult<TData> =
  | { kind: "ok"; data: TData }
  | { kind: "error"; code: string; message: string; exitCode: number };

const workflowId = "a72d7b8e-e1e8-4804-b925-355318bca593";
const feedbackLimit = 20;

const priorityOrder: Record<TaskPriority, number> = {
  critical: 0,
  high: 1,
  medium: 2,
  low: 3,
};

const taskStatusAlias: Record<TaskStatusToken, CanonicalTaskStatus> = {
  backlog: "backlog",
  todo: "backlog",
  ready: "ready",
  "in-progress": "in-progress",
  in_progress: "in-progress",
  blocked: "blocked",
  "on-hold": "on-hold",
  on_hold: "on-hold",
  done: "done",
  cancelled: "cancelled",
};

const activeTaskStates = new Set<CanonicalTaskStatus>([
  "backlog",
  "ready",
  "in-progress",
  "blocked",
  "on-hold",
]);

const initialTasks: TaskQueueItem[] = [
  {
    id: "TASK-014",
    title: "Build task/workflow control center interface",
    priority: "high",
    status: "in_progress",
    assignee: "sam.ishukri",
    dependencyIds: [],
    checklist: { done: 2, total: 5 },
    updatedAt: "2026-02-26T01:49:12Z",
  },
  {
    id: "TASK-016",
    title: "Add high-risk action safeguards",
    priority: "high",
    status: "todo",
    assignee: "unassigned",
    dependencyIds: ["TASK-014"],
    checklist: { done: 0, total: 4 },
    updatedAt: "2026-02-26T01:43:40Z",
  },
  {
    id: "TASK-019",
    title: "Structured observability diagnostics",
    priority: "medium",
    status: "done",
    assignee: "sam.ishukri",
    dependencyIds: [],
    checklist: { done: 5, total: 5 },
    updatedAt: "2026-02-25T22:08:11Z",
  },
];

const initialTimeline: PhaseTimelineEntry[] = [
  {
    id: "tl-014-001",
    checkpointOrder: 1,
    checkpointKey: "planning.01.start",
    phase: "planning",
    state: "checkpoint",
    timestamp: "2026-02-26T00:58:11Z",
    checkpointState: "approved",
  },
  {
    id: "tl-014-002",
    checkpointOrder: 2,
    checkpointKey: "planning.02.decision",
    phase: "planning",
    state: "decision",
    timestamp: "2026-02-26T01:10:53Z",
    decision: "Split queue UX and telemetry scope to keep deterministic feedback clear",
  },
  {
    id: "tl-014-003",
    checkpointOrder: 3,
    checkpointKey: "design.03.approved",
    phase: "design",
    state: "approved",
    timestamp: "2026-02-26T01:29:04Z",
    approver: "reviewer.ops",
    checkpointState: "approved",
  },
  {
    id: "tl-014-004",
    checkpointOrder: 4,
    checkpointKey: "wireframe.04.review",
    phase: "wireframe",
    state: "blocked",
    timestamp: "2026-02-26T01:47:29Z",
    checkpointState: "pending",
    blockerReason: "reviewer approval missing for wireframe artifacts",
  },
];

const initialTelemetry: TelemetryEntry[] = [
  {
    id: "tele-014-0010",
    timestamp: "2026-02-26T01:51:56Z",
    action: "workflow.pause",
    endpoint: "POST /api/v1/workflows/:id/pause",
    actor: "sam.ishukri",
    correlationId: "ao-corr-014-0011",
    outcome: "idempotent-retry",
    message: "no-op retry accepted; request was already pending",
  },
  {
    id: "tele-014-0011",
    timestamp: "2026-02-26T01:52:01Z",
    action: "workflow.pause",
    endpoint: "POST /api/v1/workflows/:id/pause",
    actor: "sam.ishukri",
    correlationId: "ao-corr-014-0011",
    outcome: "success",
    message: "workflow paused and dispatch loop parked",
  },
];

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

export function parseAoEnvelope<TData>(value: unknown): ApiResult<TData> {
  if (!isRecord(value) || value.schema !== "ao.cli.v1" || typeof value.ok !== "boolean") {
    return {
      kind: "error",
      code: "invalid_envelope",
      message: "Response does not match ao.cli.v1 envelope shape.",
      exitCode: 1,
    };
  }

  if (value.ok) {
    return {
      kind: "ok",
      data: (value as AoSuccessEnvelope<TData>).data,
    };
  }

  const error = (value as AoErrorEnvelope).error;
  return {
    kind: "error",
    code: error.code,
    message: error.message,
    exitCode: error.exit_code,
  };
}

function toEpoch(iso: string): number {
  const parsed = Date.parse(iso);
  return Number.isNaN(parsed) ? 0 : parsed;
}

function toTimestampLabel(iso: string): string {
  const parsed = Date.parse(iso);
  if (Number.isNaN(parsed)) {
    return iso;
  }
  return `${new Date(parsed).toISOString().slice(0, 19)} UTC`;
}

function canonicalTaskStatus(status: TaskStatusToken): CanonicalTaskStatus {
  return taskStatusAlias[status];
}

function sortQueue(tasks: TaskQueueItem[]): TaskQueueItem[] {
  return [...tasks].sort((left, right) => {
    const priorityDelta = priorityOrder[left.priority] - priorityOrder[right.priority];
    if (priorityDelta !== 0) {
      return priorityDelta;
    }

    const updatedDelta = toEpoch(right.updatedAt) - toEpoch(left.updatedAt);
    if (updatedDelta !== 0) {
      return updatedDelta;
    }

    return left.id.localeCompare(right.id);
  });
}

function filterQueue(
  tasks: TaskQueueItem[],
  statusFilter: CanonicalTaskStatus | "all",
  searchQuery: string,
): TaskQueueItem[] {
  const trimmedQuery = searchQuery.trim().toLowerCase();
  return tasks.filter((task) => {
    const canonical = canonicalTaskStatus(task.status);
    if (statusFilter !== "all" && canonical !== statusFilter) {
      return false;
    }

    if (!trimmedQuery) {
      return true;
    }

    const haystack = `${task.id} ${task.title} ${task.assignee}`.toLowerCase();
    return haystack.includes(trimmedQuery);
  });
}

function sortTimeline(entries: PhaseTimelineEntry[]): PhaseTimelineEntry[] {
  return [...entries].sort((left, right) => {
    if (left.checkpointOrder !== right.checkpointOrder) {
      return left.checkpointOrder - right.checkpointOrder;
    }

    const timestampDelta = toEpoch(left.timestamp) - toEpoch(right.timestamp);
    if (timestampDelta !== 0) {
      return timestampDelta;
    }

    return left.id.localeCompare(right.id);
  });
}

function checklistComplete(checklist: TaskChecklist): boolean {
  return checklist.total > 0 && checklist.done >= checklist.total;
}

function hasUnfinishedDependency(task: TaskQueueItem, index: Map<string, TaskQueueItem>): boolean {
  return task.dependencyIds.some((dependencyId) => {
    const dependency = index.get(dependencyId);
    return !dependency || canonicalTaskStatus(dependency.status) !== "done";
  });
}

function taskTransitionAvailability(
  task: TaskQueueItem,
  nextStatus: CanonicalTaskStatus,
  index: Map<string, TaskQueueItem>,
): ActionAvailability {
  const currentStatus = canonicalTaskStatus(task.status);

  if (currentStatus === nextStatus) {
    return {
      enabled: false,
      reason: "already in selected status",
      requiresConfirmation: false,
    };
  }

  if (nextStatus === "in-progress" && hasUnfinishedDependency(task, index)) {
    return {
      enabled: false,
      reason: "blocked by unresolved dependency",
      requiresConfirmation: false,
    };
  }

  if (nextStatus === "done" && !checklistComplete(task.checklist)) {
    return {
      enabled: false,
      reason: "finish checklist before moving to done",
      requiresConfirmation: false,
    };
  }

  if (nextStatus === "cancelled" && !activeTaskStates.has(currentStatus)) {
    return {
      enabled: false,
      reason: "only active tasks can transition to cancelled from this surface",
      requiresConfirmation: false,
    };
  }

  if (nextStatus === "cancelled") {
    return {
      enabled: true,
      reason: null,
      requiresConfirmation: true,
    };
  }

  return {
    enabled: true,
    reason: null,
    requiresConfirmation: false,
  };
}

function workflowActionAvailability(
  action: WorkflowAction,
  workflowStatus: WorkflowStatus,
  gateState: GateState,
  pendingAction: WorkflowAction | null,
): ActionAvailability {
  if (pendingAction === action) {
    return {
      enabled: false,
      reason: "already pending",
      requiresConfirmation: false,
    };
  }

  switch (action) {
    case "workflow.run":
      if (workflowStatus !== "idle") {
        return {
          enabled: false,
          reason: "workflow already started",
          requiresConfirmation: false,
        };
      }

      if (gateState !== "approved") {
        return {
          enabled: false,
          reason: "gate approval missing",
          requiresConfirmation: false,
        };
      }

      return { enabled: true, reason: null, requiresConfirmation: false };

    case "workflow.resume":
      return workflowStatus === "paused"
        ? { enabled: true, reason: null, requiresConfirmation: false }
        : {
            enabled: false,
            reason: "workflow is not paused",
            requiresConfirmation: false,
          };

    case "workflow.pause":
      return workflowStatus === "running"
        ? { enabled: true, reason: null, requiresConfirmation: false }
        : {
            enabled: false,
            reason: "workflow is not running",
            requiresConfirmation: false,
          };

    case "workflow.cancel":
      return workflowStatus === "running" || workflowStatus === "paused"
        ? { enabled: true, reason: null, requiresConfirmation: true }
        : {
            enabled: false,
            reason: "workflow is already terminal",
            requiresConfirmation: false,
          };

    default:
      return {
        enabled: false,
        reason: "unsupported action",
        requiresConfirmation: false,
      };
  }
}

function nextWorkflowStatus(action: WorkflowAction, current: WorkflowStatus): WorkflowStatus {
  switch (action) {
    case "workflow.run":
    case "workflow.resume":
      return "running";
    case "workflow.pause":
      return "paused";
    case "workflow.cancel":
      return current === "completed" ? "completed" : "cancelled";
    default:
      return current;
  }
}

function newCorrelation(seed: number): string {
  return `ao-corr-014-${seed.toString().padStart(4, "0")}`;
}

function appendTelemetry(current: TelemetryEntry[], next: TelemetryEntry): TelemetryEntry[] {
  const merged = [...current, next];
  return merged.length > feedbackLimit ? merged.slice(merged.length - feedbackLimit) : merged;
}

function actionEndpoint(action: string): string {
  if (action.startsWith("task.transition.")) {
    return "POST /api/v1/tasks/:id/status";
  }

  switch (action) {
    case "workflow.run":
      return "POST /api/v1/workflows/run";
    case "workflow.resume":
      return "POST /api/v1/workflows/:id/resume";
    case "workflow.pause":
      return "POST /api/v1/workflows/:id/pause";
    case "workflow.cancel":
      return "POST /api/v1/workflows/:id/cancel";
    default:
      return "unknown";
  }
}

function buildWorkflowCancelGate(targetWorkflowId: string | null): ConfirmationGate | null {
  if (!targetWorkflowId) {
    return null;
  }

  return {
    kind: "workflow.cancel",
    targetId: targetWorkflowId,
    phrase: `CANCEL WORKFLOW ${targetWorkflowId}`,
  };
}

function buildTaskCancelGate(taskId: string | null): ConfirmationGate | null {
  if (!taskId) {
    return null;
  }

  return {
    kind: "task.transition.cancelled",
    targetId: taskId,
    phrase: `CANCEL TASK ${taskId}`,
  };
}

export function WorkflowControlCenterWireframe() {
  const [tasks, setTasks] = useState<TaskQueueItem[]>(initialTasks);
  const [workflowStatus, setWorkflowStatus] = useState<WorkflowStatus>("running");
  const [gateState] = useState<GateState>("pending");
  const [phaseTimeline] = useState<PhaseTimelineEntry[]>(initialTimeline);
  const [telemetry, setTelemetry] = useState<TelemetryEntry[]>(initialTelemetry);
  const [pendingWorkflowAction, setPendingWorkflowAction] = useState<WorkflowAction | null>(null);
  const [pendingTaskTransition, setPendingTaskTransition] = useState<string | null>(null);
  const [statusFilter, setStatusFilter] = useState<CanonicalTaskStatus | "all">("all");
  const [searchQuery, setSearchQuery] = useState("");
  const [gateRequest, setGateRequest] = useState<ConfirmationGate | null>(null);
  const [typedConfirmation, setTypedConfirmation] = useState("");
  const correlationSeedRef = useRef(12);

  const taskIndex = useMemo(() => new Map(tasks.map((task) => [task.id, task])), [tasks]);
  const queue = useMemo(() => sortQueue(tasks), [tasks]);
  const filteredQueue = useMemo(
    () => filterQueue(queue, statusFilter, searchQuery),
    [queue, searchQuery, statusFilter],
  );
  const orderedTimeline = useMemo(() => sortTimeline(phaseTimeline), [phaseTimeline]);

  const queueState: QueueState =
    filteredQueue.length > 0 ? "ready" : queue.length === 0 ? "empty" : "filtered-empty";
  const typedConfirmationValid = gateRequest
    ? typedConfirmation.trim() === gateRequest.phrase
    : false;

  const takeCorrelationId = () => {
    const next = newCorrelation(correlationSeedRef.current);
    correlationSeedRef.current += 1;
    return next;
  };

  const publishTelemetry = (entry: Omit<TelemetryEntry, "id">) => {
    setTelemetry((current) =>
      appendTelemetry(current, {
        id: `tele-${entry.correlationId}-${entry.action}-${entry.outcome}-${Date.parse(entry.timestamp)}`,
        ...entry,
      }),
    );
  };

  const dispatchTaskTransition = (taskId: string, nextStatus: CanonicalTaskStatus) => {
    const action = `task.transition.${nextStatus}`;
    const transitionKey = `${taskId}:${nextStatus}`;

    if (pendingTaskTransition === transitionKey) {
      publishTelemetry({
        timestamp: new Date().toISOString(),
        action,
        endpoint: actionEndpoint(action),
        actor: "sam.ishukri",
        correlationId: takeCorrelationId(),
        outcome: "idempotent-retry",
        message: "duplicate task transition suppressed while request is pending",
      });
      return;
    }

    const correlationId = takeCorrelationId();
    setPendingTaskTransition(transitionKey);

    setTasks((current) =>
      current.map((item) =>
        item.id === taskId
          ? {
              ...item,
              status: nextStatus,
              updatedAt: new Date().toISOString(),
            }
          : item,
      ),
    );

    setPendingTaskTransition(null);
    setGateRequest(null);
    setTypedConfirmation("");

    publishTelemetry({
      timestamp: new Date().toISOString(),
      action,
      endpoint: actionEndpoint(action),
      actor: "sam.ishukri",
      correlationId,
      outcome: "success",
      message: `${taskId} transitioned to ${nextStatus}`,
    });
  };

  const onTaskTransition = (taskId: string, nextStatus: CanonicalTaskStatus) => {
    const task = taskIndex.get(taskId);
    const action = `task.transition.${nextStatus}`;

    if (!task) {
      publishTelemetry({
        timestamp: new Date().toISOString(),
        action,
        endpoint: actionEndpoint(action),
        actor: "sam.ishukri",
        correlationId: takeCorrelationId(),
        outcome: "conflict",
        message: "task not found; transition blocked",
      });
      return;
    }

    const availability = taskTransitionAvailability(task, nextStatus, taskIndex);
    if (!availability.enabled) {
      publishTelemetry({
        timestamp: new Date().toISOString(),
        action,
        endpoint: actionEndpoint(action),
        actor: "sam.ishukri",
        correlationId: takeCorrelationId(),
        outcome: "conflict",
        message: availability.reason ?? "transition rejected",
      });
      return;
    }

    if (availability.requiresConfirmation) {
      const gate = buildTaskCancelGate(task.id);
      if (!gate) {
        publishTelemetry({
          timestamp: new Date().toISOString(),
          action,
          endpoint: actionEndpoint(action),
          actor: "sam.ishukri",
          correlationId: takeCorrelationId(),
          outcome: "conflict",
          message: "gate metadata missing; transition blocked (fail-closed)",
        });
        return;
      }

      setGateRequest(gate);
      setTypedConfirmation("");
      return;
    }

    dispatchTaskTransition(taskId, nextStatus);
  };

  const dispatchWorkflowAction = (action: WorkflowAction) => {
    if (pendingWorkflowAction === action) {
      publishTelemetry({
        timestamp: new Date().toISOString(),
        action,
        endpoint: actionEndpoint(action),
        actor: "sam.ishukri",
        correlationId: takeCorrelationId(),
        outcome: "idempotent-retry",
        message: "duplicate workflow request suppressed while action is pending",
      });
      return;
    }

    const correlationId = takeCorrelationId();
    setPendingWorkflowAction(action);

    const availability = workflowActionAvailability(action, workflowStatus, gateState, null);
    if (!availability.enabled) {
      setPendingWorkflowAction(null);
      publishTelemetry({
        timestamp: new Date().toISOString(),
        action,
        endpoint: actionEndpoint(action),
        actor: "sam.ishukri",
        correlationId,
        outcome: "conflict",
        message: availability.reason ?? "action unavailable",
      });
      return;
    }

    setWorkflowStatus((current) => nextWorkflowStatus(action, current));
    setPendingWorkflowAction(null);
    setGateRequest(null);
    setTypedConfirmation("");

    publishTelemetry({
      timestamp: new Date().toISOString(),
      action,
      endpoint: actionEndpoint(action),
      actor: "sam.ishukri",
      correlationId,
      outcome: "success",
      message: `${action} applied successfully`,
    });
  };

  const onRequestWorkflowAction = (action: WorkflowAction) => {
    const availability = workflowActionAvailability(
      action,
      workflowStatus,
      gateState,
      pendingWorkflowAction,
    );

    if (!availability.enabled) {
      publishTelemetry({
        timestamp: new Date().toISOString(),
        action,
        endpoint: actionEndpoint(action),
        actor: "sam.ishukri",
        correlationId: takeCorrelationId(),
        outcome: availability.reason === "already pending" ? "idempotent-retry" : "conflict",
        message: availability.reason ?? "action unavailable",
      });
      return;
    }

    if (availability.requiresConfirmation) {
      const gate = buildWorkflowCancelGate(workflowId);
      if (!gate) {
        publishTelemetry({
          timestamp: new Date().toISOString(),
          action,
          endpoint: actionEndpoint(action),
          actor: "sam.ishukri",
          correlationId: takeCorrelationId(),
          outcome: "conflict",
          message: "gate metadata missing; workflow cancel blocked (fail-closed)",
        });
        return;
      }

      setGateRequest(gate);
      setTypedConfirmation("");
      return;
    }

    dispatchWorkflowAction(action);
  };

  const onConfirmGate = () => {
    if (!gateRequest) {
      return;
    }

    const action = gateRequest.kind;
    if (!typedConfirmationValid) {
      publishTelemetry({
        timestamp: new Date().toISOString(),
        action,
        endpoint: actionEndpoint(action),
        actor: "sam.ishukri",
        correlationId: takeCorrelationId(),
        outcome: "conflict",
        message: "confirmation phrase mismatch",
      });
      return;
    }

    if (gateRequest.kind === "workflow.cancel") {
      dispatchWorkflowAction("workflow.cancel");
      return;
    }

    dispatchTaskTransition(gateRequest.targetId, "cancelled");
  };

  const onDismissGate = () => {
    setGateRequest(null);
    setTypedConfirmation("");
  };

  return (
    <section aria-label="Workflow control center wireframe">
      <h1>Task and Workflow Control Center</h1>
      <p>Queue-first controls for /tasks and /workflows with deterministic safety rails.</p>

      <TaskQueueCard
        tasks={filteredQueue}
        taskIndex={taskIndex}
        queueState={queueState}
        statusFilter={statusFilter}
        searchQuery={searchQuery}
        pendingTransition={pendingTaskTransition}
        onStatusFilterChange={setStatusFilter}
        onSearchQueryChange={setSearchQuery}
        onTransition={onTaskTransition}
      />

      <WorkflowActionsPanel
        workflowStatus={workflowStatus}
        gateState={gateState}
        pendingAction={pendingWorkflowAction}
        onRequestAction={onRequestWorkflowAction}
      />

      {gateRequest ? (
        <section role="dialog" aria-modal="true" aria-label="High impact confirmation gate">
          <h2>Confirm {gateRequest.kind}</h2>
          <p>
            Target: <code>{gateRequest.targetId}</code>
          </p>
          <p>
            Type <code>{gateRequest.phrase}</code> exactly to continue.
          </p>
          <label>
            Confirmation phrase
            <input
              value={typedConfirmation}
              onChange={(event) => setTypedConfirmation(event.target.value)}
              aria-invalid={!typedConfirmationValid}
            />
          </label>
          {!typedConfirmationValid ? (
            <p role="alert">Phrase mismatch. Dispatch remains blocked.</p>
          ) : null}
          <div>
            <button type="button" onClick={onDismissGate}>
              Cancel
            </button>
            <button type="button" onClick={onConfirmGate} disabled={!typedConfirmationValid}>
              Confirm high-impact action
            </button>
          </div>
        </section>
      ) : null}

      <PhaseTimelinePanel entries={orderedTimeline} />
      <TelemetryPanel entries={telemetry} />
    </section>
  );
}

function TaskQueueCard(props: {
  tasks: TaskQueueItem[];
  taskIndex: Map<string, TaskQueueItem>;
  queueState: QueueState;
  statusFilter: CanonicalTaskStatus | "all";
  searchQuery: string;
  pendingTransition: string | null;
  onStatusFilterChange: (value: CanonicalTaskStatus | "all") => void;
  onSearchQueryChange: (value: string) => void;
  onTransition: (taskId: string, nextStatus: CanonicalTaskStatus) => void;
}) {
  return (
    <section aria-label="Task queue wireframe">
      <h2>Prioritized queue</h2>
      <p>Sort contract: priority, then newest updated_at, then task ID.</p>

      <label>
        Status filter
        <select
          value={props.statusFilter}
          onChange={(event) =>
            props.onStatusFilterChange(event.target.value as CanonicalTaskStatus | "all")
          }
        >
          <option value="all">all</option>
          <option value="backlog">backlog</option>
          <option value="ready">ready</option>
          <option value="in-progress">in-progress</option>
          <option value="blocked">blocked</option>
          <option value="on-hold">on-hold</option>
          <option value="done">done</option>
          <option value="cancelled">cancelled</option>
        </select>
      </label>

      <label>
        Search
        <input
          type="search"
          value={props.searchQuery}
          onChange={(event) => props.onSearchQueryChange(event.target.value)}
          placeholder="Find by ID, title, or owner"
        />
      </label>

      {props.queueState === "empty" ? <p role="status">Queue is empty.</p> : null}
      {props.queueState === "filtered-empty" ? (
        <p role="status">No queue rows match the active filter/search criteria.</p>
      ) : null}

      <ul>
        {props.tasks.map((task) => {
          const canonicalStatus = canonicalTaskStatus(task.status);
          const startAvailability = taskTransitionAvailability(task, "in-progress", props.taskIndex);
          const doneAvailability = taskTransitionAvailability(task, "done", props.taskIndex);
          const cancelAvailability = taskTransitionAvailability(task, "cancelled", props.taskIndex);
          const startKey = `${task.id}:in-progress`;
          const doneKey = `${task.id}:done`;
          const cancelKey = `${task.id}:cancelled`;

          return (
            <li key={task.id}>
              <h3>{task.id}</h3>
              <p>{task.title}</p>
              <p>
                priority={task.priority}; status={canonicalStatus}; owner={task.assignee}
              </p>
              <p>
                checklist={task.checklist.done}/{task.checklist.total}; dependencies=
                {task.dependencyIds.length > 0 ? task.dependencyIds.join(",") : "none"}
              </p>

              <button
                type="button"
                onClick={() => props.onTransition(task.id, "in-progress")}
                disabled={!startAvailability.enabled || props.pendingTransition === startKey}
              >
                Set status to in-progress
              </button>
              {!startAvailability.enabled && startAvailability.reason ? (
                <p>disabled: {startAvailability.reason}</p>
              ) : null}

              <button
                type="button"
                onClick={() => props.onTransition(task.id, "done")}
                disabled={!doneAvailability.enabled || props.pendingTransition === doneKey}
              >
                Set status to done
              </button>
              {!doneAvailability.enabled && doneAvailability.reason ? (
                <p>disabled: {doneAvailability.reason}</p>
              ) : null}

              <button
                type="button"
                onClick={() => props.onTransition(task.id, "cancelled")}
                disabled={!cancelAvailability.enabled || props.pendingTransition === cancelKey}
              >
                Set status to cancelled
              </button>
              {!cancelAvailability.enabled && cancelAvailability.reason ? (
                <p>disabled: {cancelAvailability.reason}</p>
              ) : null}
            </li>
          );
        })}
      </ul>
    </section>
  );
}

function WorkflowActionsPanel(props: {
  workflowStatus: WorkflowStatus;
  gateState: GateState;
  pendingAction: WorkflowAction | null;
  onRequestAction: (action: WorkflowAction) => void;
}) {
  const run = workflowActionAvailability(
    "workflow.run",
    props.workflowStatus,
    props.gateState,
    props.pendingAction,
  );
  const resume = workflowActionAvailability(
    "workflow.resume",
    props.workflowStatus,
    props.gateState,
    props.pendingAction,
  );
  const pause = workflowActionAvailability(
    "workflow.pause",
    props.workflowStatus,
    props.gateState,
    props.pendingAction,
  );
  const cancel = workflowActionAvailability(
    "workflow.cancel",
    props.workflowStatus,
    props.gateState,
    props.pendingAction,
  );

  return (
    <section aria-label="Workflow controls wireframe">
      <h2>Workflow actions</h2>
      <p>
        lifecycle={props.workflowStatus}; gate={props.gateState}; pending=
        {props.pendingAction ?? "none"}
      </p>

      <button type="button" onClick={() => props.onRequestAction("workflow.run")} disabled={!run.enabled}>
        Run workflow
      </button>
      {!run.enabled && run.reason ? <p>disabled: {run.reason}</p> : null}

      <button
        type="button"
        onClick={() => props.onRequestAction("workflow.resume")}
        disabled={!resume.enabled}
      >
        Resume workflow
      </button>
      {!resume.enabled && resume.reason ? <p>disabled: {resume.reason}</p> : null}

      <button type="button" onClick={() => props.onRequestAction("workflow.pause")} disabled={!pause.enabled}>
        Pause workflow
      </button>
      {!pause.enabled && pause.reason ? <p>disabled: {pause.reason}</p> : null}

      <button
        type="button"
        onClick={() => props.onRequestAction("workflow.cancel")}
        disabled={!cancel.enabled}
      >
        Cancel workflow
      </button>
      {!cancel.enabled && cancel.reason ? <p>disabled: {cancel.reason}</p> : null}
    </section>
  );
}

function PhaseTimelinePanel(props: { entries: PhaseTimelineEntry[] }) {
  if (props.entries.length === 0) {
    return (
      <section aria-label="Phase timeline wireframe">
        <h2>Phase timeline</h2>
        <p>Timeline is empty. No checkpoints or decisions are available.</p>
      </section>
    );
  }

  return (
    <section aria-label="Phase timeline wireframe">
      <h2>Phase timeline</h2>
      <p>Order: checkpoint order, then timestamp, then stable key.</p>
      <ol>
        {props.entries.map((entry) => (
          <li key={entry.id}>
            <p>
              checkpoint={entry.checkpointKey}; phase={entry.phase}; state={entry.state}
            </p>
            <p>at={toTimestampLabel(entry.timestamp)}</p>
            {entry.checkpointState ? <p>checkpoint_state={entry.checkpointState}</p> : null}
            {entry.approver ? <p>approver={entry.approver}</p> : null}
            {entry.decision ? <p>decision={entry.decision}</p> : null}
            {entry.blockerReason ? <p>blocked={entry.blockerReason}</p> : null}
          </li>
        ))}
      </ol>
    </section>
  );
}

function TelemetryPanel(props: { entries: TelemetryEntry[] }) {
  const latest = props.entries.length > 0 ? props.entries[props.entries.length - 1] : null;

  return (
    <section aria-label="Control center telemetry wireframe">
      <h2>Telemetry feedback</h2>
      {latest ? (
        <p aria-live="polite">
          Latest event: {latest.action} at {toTimestampLabel(latest.timestamp)} ({latest.outcome})
        </p>
      ) : (
        <p aria-live="polite">No feedback events yet.</p>
      )}
      <ol>
        {props.entries.map((entry) => (
          <li key={entry.id} role={entry.outcome === "conflict" ? "alert" : undefined}>
            <p>
              {toTimestampLabel(entry.timestamp)} | {entry.action} | {entry.outcome}
            </p>
            <p>{entry.endpoint}</p>
            <p>{entry.message}</p>
            <p>
              actor={entry.actor}; correlation={entry.correlationId}
            </p>
          </li>
        ))}
      </ol>
    </section>
  );
}
