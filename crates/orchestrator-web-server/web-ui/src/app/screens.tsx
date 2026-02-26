import { FormEvent, ReactNode, useEffect, useMemo, useState } from "react";
import { Link, useParams } from "react-router-dom";

import { ActionGateConfig, ActionGateDialog, matchesConfirmationPhrase } from "./action-gate-dialog";
import { useProjectContext } from "./project-context";
import { DiagnosticsPanel } from "./diagnostics-panel";
import { api, firstApiError, RequestJsonValue } from "../lib/api/client";
import { ApiError } from "../lib/api/envelope";
import type {
  PriorityValue,
  TaskDetail,
  TaskStatsPayload,
  TaskStatusValue,
  TaskSummary,
  WorkflowCheckpoint,
  WorkflowDecision,
  WorkflowStatusValue,
  WorkflowSummary,
} from "../lib/api/contracts/models";
import { ResourceState, useApiResource } from "../lib/api/use-api-resource";
import { useDaemonEvents } from "../lib/events/use-daemon-events";

type DaemonAction = "start" | "pause" | "resume" | "stop" | "clear";

type DaemonActionConfig = {
  label: string;
  method: "POST" | "DELETE";
  path: string;
  impact: string;
  confirmationPhrase?: string;
};

type ActionRecord = {
  id: string;
  action: DaemonAction;
  mode: "execute" | "dry-run";
  outcome: "ok" | "error" | "preview";
  timestamp: string;
  detail: string;
};

const DAEMON_ACTION_CONFIG: Record<DaemonAction, DaemonActionConfig> = {
  start: {
    label: "Start",
    method: "POST",
    path: "/api/v1/daemon/start",
    impact: "Starts daemon processing.",
  },
  pause: {
    label: "Pause",
    method: "POST",
    path: "/api/v1/daemon/pause",
    impact: "Pauses new daemon scheduling work.",
  },
  resume: {
    label: "Resume",
    method: "POST",
    path: "/api/v1/daemon/resume",
    impact: "Resumes daemon scheduling work.",
  },
  stop: {
    label: "Stop",
    method: "POST",
    path: "/api/v1/daemon/stop",
    impact: "Stops the daemon and can interrupt active scheduling.",
    confirmationPhrase: "STOP",
  },
  clear: {
    label: "Clear Logs",
    method: "DELETE",
    path: "/api/v1/daemon/logs",
    impact: "Permanently clears daemon log history shown in the UI.",
    confirmationPhrase: "CLEAR LOGS",
  },
};

const CONTROL_FEEDBACK_LIMIT = 20;

const TASK_STATUS_OPTIONS = [
  "backlog",
  "ready",
  "in-progress",
  "blocked",
  "on-hold",
  "done",
  "cancelled",
] as const;

type QueueTaskStatus = (typeof TASK_STATUS_OPTIONS)[number];

const ACTIVE_TASK_STATUSES = new Set<QueueTaskStatus>([
  "backlog",
  "ready",
  "in-progress",
  "blocked",
  "on-hold",
]);

const PRIORITY_ORDER: Record<Exclude<PriorityValue, "unknown">, number> = {
  critical: 0,
  high: 1,
  medium: 2,
  low: 3,
};

type ControlFeedbackEntry = {
  id: string;
  action: string;
  targetId: string;
  outcome: "success" | "error";
  timestamp: string;
  message: string;
  correlationId?: string;
};

export function DashboardPage() {
  const state = useApiResource(
    async () => {
      const [systemInfo, daemonStatus, activeProject, taskStats] = await Promise.all([
        api.systemInfo(),
        api.daemonStatus(),
        api.projectsActive(),
        api.tasksStats(),
      ]);

      const error = firstApiError(systemInfo, daemonStatus, activeProject, taskStats);
      if (error) {
        return error;
      }

      return {
        kind: "ok" as const,
        data: {
          systemInfo: systemInfo.data,
          daemonStatus: daemonStatus.data,
          activeProject: activeProject.data,
          taskStats: taskStats.data,
        },
      };
    },
    [],
  );

  return (
    <RouteSection title="Dashboard" description="Global status overview for AO daemon and workspace.">
      <ResourceStateView
        state={state}
        emptyMessage="No dashboard data returned."
        render={(data) => (
          <div className="grid two">
            <JsonPanel title="System Info" data={data.systemInfo} />
            <JsonPanel title="Daemon Status" data={data.daemonStatus} />
            <JsonPanel title="Active Project" data={data.activeProject} />
            <JsonPanel title="Task Stats" data={data.taskStats} />
          </div>
        )}
      />
    </RouteSection>
  );
}

export function DaemonPage() {
  const [refreshNonce, setRefreshNonce] = useState(0);
  const [pendingAction, setPendingAction] = useState<DaemonAction | null>(null);
  const [confirmationInput, setConfirmationInput] = useState("");
  const [dryRunMode, setDryRunMode] = useState(true);
  const [actionRecords, setActionRecords] = useState<ActionRecord[]>([]);
  const [actionState, setActionState] = useState<
    { kind: "idle" } | { kind: "ok"; message: string } | { kind: "error"; message: string }
  >({ kind: "idle" });

  const healthState = useApiResource(
    async () => api.daemonHealth(),
    [refreshNonce],
  );

  const logsState = useApiResource(
    async () => api.daemonLogs(100),
    [refreshNonce],
    {
      isEmpty: (data) => Array.isArray(data) && data.length === 0,
    },
  );

  const runAction = (action: DaemonAction, runAsDryRun = false) => {
    const config = DAEMON_ACTION_CONFIG[action];
    if (runAsDryRun) {
      const previewMessage = `Dry-run preview completed for ${config.label}.`;
      setActionState({ kind: "ok", message: previewMessage });
      setActionRecords((current) => [
        {
          id: `${Date.now()}-${action}-preview`,
          action,
          mode: "dry-run",
          outcome: "preview",
          timestamp: new Date().toISOString(),
          detail: `${config.method} ${config.path} (${config.impact})`,
        },
        ...current,
      ]);
      setPendingAction(null);
      setConfirmationInput("");
      return;
    }

    const request =
      action === "start"
        ? api.daemonStart()
        : action === "pause"
          ? api.daemonPause()
          : action === "resume"
            ? api.daemonResume()
            : action === "stop"
              ? api.daemonStop()
              : api.daemonClearLogs();

    void request.then((result) => {
      if (result.kind === "error") {
        setActionState({ kind: "error", message: `${result.code}: ${result.message}` });
        setActionRecords((current) => [
          {
            id: `${Date.now()}-${action}-error`,
            action,
            mode: "execute",
            outcome: "error",
            timestamp: new Date().toISOString(),
            detail: `${result.code}: ${result.message}`,
          },
          ...current,
        ]);
        return;
      }

      setActionState({ kind: "ok", message: `${config.label} completed.` });
      setActionRecords((current) => [
        {
          id: `${Date.now()}-${action}-ok`,
          action,
          mode: "execute",
          outcome: "ok",
          timestamp: new Date().toISOString(),
          detail: `${config.method} ${config.path}`,
        },
        ...current,
      ]);
      setRefreshNonce((current) => current + 1);
      setPendingAction(null);
      setConfirmationInput("");
    });
  };

  const requestAction = (action: DaemonAction) => {
    const config = DAEMON_ACTION_CONFIG[action];
    if (!config.confirmationPhrase) {
      runAction(action, false);
      return;
    }

    setPendingAction(action);
    setConfirmationInput("");
    setDryRunMode(true);
  };

  const pendingConfig = pendingAction ? DAEMON_ACTION_CONFIG[pendingAction] : null;
  const canConfirm = pendingConfig?.confirmationPhrase
    ? matchesConfirmationPhrase(confirmationInput, pendingConfig.confirmationPhrase)
    : false;

  return (
    <RouteSection title="Daemon" description="Control daemon state, health, and log stream.">
      <div className="panel-actions">
        <button type="button" onClick={() => requestAction("start")}>
          Start
        </button>
        <button type="button" onClick={() => requestAction("pause")}>
          Pause
        </button>
        <button type="button" onClick={() => requestAction("resume")}>
          Resume
        </button>
        <button type="button" className="danger-action" onClick={() => requestAction("stop")}>
          Stop
        </button>
        <button type="button" className="danger-action" onClick={() => requestAction("clear")}>
          Clear Logs
        </button>
      </div>

      {pendingConfig ? (
        <section className="safeguard-panel" aria-label="Action safeguards">
          <h2>Review High-Risk Action</h2>
          <p>
            <strong>Action:</strong> {pendingConfig.label}
          </p>
          <p>
            <strong>Preview:</strong> <code>{pendingConfig.method}</code> <code>{pendingConfig.path}</code>
          </p>
          <p>{pendingConfig.impact}</p>
          <label>
            Confirmation phrase
            <input
              value={confirmationInput}
              onChange={(event) => setConfirmationInput(event.target.value)}
              placeholder={pendingConfig.confirmationPhrase}
              aria-describedby="confirmation-help"
            />
          </label>
          <p id="confirmation-help">
            Type <code>{pendingConfig.confirmationPhrase}</code> to enable confirmation.
          </p>
          <label className="inline-checkbox">
            <input
              type="checkbox"
              checked={dryRunMode}
              onChange={(event) => setDryRunMode(event.target.checked)}
            />
            Preview only (dry-run, no API call)
          </label>
          <div className="panel-actions">
            <button type="button" disabled={!canConfirm} onClick={() => runAction(pendingAction!, dryRunMode)}>
              {dryRunMode ? "Run Dry-Run Preview" : "Confirm and Execute"}
            </button>
            <button
              type="button"
              onClick={() => {
                setPendingAction(null);
                setConfirmationInput("");
              }}
            >
              Cancel
            </button>
          </div>
        </section>
      ) : null}

      {actionState.kind === "ok" ? (
        <p role="status" aria-live="polite">
          {actionState.message}
        </p>
      ) : null}
      {actionState.kind === "error" ? (
        <ErrorState
          error={{
            kind: "error",
            code: "daemon_action_failed",
            message: actionState.message,
            exitCode: 1,
          }}
        />
      ) : null}

      <DiagnosticsPanel title="Daemon Diagnostics" actionPrefixes={["daemon."]} />

      <div className="grid two">
        <div className="panel">
          <h2>Action Audit Trail</h2>
          {actionRecords.length === 0 ? (
            <EmptyState message="No daemon actions recorded in this session." />
          ) : (
            <pre>{JSON.stringify(actionRecords.slice(0, 20), null, 2)}</pre>
          )}
        </div>

        <ResourceStateView
          state={healthState}
          emptyMessage="No daemon health response available."
          render={(data) => <JsonPanel title="Health" data={data} />}
        />

        <ResourceStateView
          state={logsState}
          emptyMessage="No daemon logs returned yet."
          render={(data) => <JsonPanel title="Logs" data={data} />}
        />
      </div>
    </RouteSection>
  );
}

export function ProjectsPage() {
  const { activeProjectId, projects, source } = useProjectContext();

  const requirementsState = useApiResource(
    async () => api.projectsRequirementsSummary(),
    [],
    {
      isEmpty: (data) => Array.isArray(data) && data.length === 0,
    },
  );

  return (
    <RouteSection
      title="Projects"
      description="Workspace projects with context selection and requirement summaries."
    >
      <p>
        Active context: <code>{activeProjectId ?? "none"}</code> (<code>{source}</code>)
      </p>
      <div className="grid two">
        <div className="panel">
          <h2>Projects</h2>
          {projects.length === 0 ? (
            <EmptyState message="No projects available in context." />
          ) : (
            <ul>
              {projects.map((project) => (
                <li key={project.id}>
                  <Link to={`/projects/${project.id}`}>{project.name}</Link>
                </li>
              ))}
            </ul>
          )}
        </div>

        <ResourceStateView
          state={requirementsState}
          emptyMessage="No requirement summary data found."
          render={(data) => <JsonPanel title="Requirement Summary" data={data} />}
        />
      </div>
    </RouteSection>
  );
}

export function ProjectDetailPage() {
  const params = useParams();
  const projectId = params.projectId ?? "";

  const state = useApiResource(
    async () => {
      const [project, tasks, workflows, requirements] = await Promise.all([
        api.projectsById(projectId),
        api.projectTasks(projectId),
        api.projectWorkflows(projectId),
        api.projectsRequirementsById(projectId),
      ]);

      const error = firstApiError(project, tasks, workflows, requirements);
      if (error) {
        return error;
      }

      return {
        kind: "ok" as const,
        data: {
          project: project.data,
          tasks: tasks.data,
          workflows: workflows.data,
          requirements: requirements.data,
        },
      };
    },
    [projectId],
  );

  return (
    <RouteSection title="Project Detail" description={`Project scope for ${projectId}.`}>
      <ResourceStateView
        state={state}
        emptyMessage="Project details are empty."
        render={(data) => (
          <div className="grid two">
            <JsonPanel title="Project" data={data.project} />
            <JsonPanel title="Tasks" data={data.tasks} />
            <JsonPanel title="Workflows" data={data.workflows} />
            <JsonPanel title="Requirements" data={data.requirements} />
          </div>
        )}
      />
    </RouteSection>
  );
}

export function RequirementDetailPage() {
  const params = useParams();
  const projectId = params.projectId ?? "";
  const requirementId = params.requirementId ?? "";

  const state = useApiResource(
    async () => api.projectRequirementDetail(projectId, requirementId),
    [projectId, requirementId],
  );

  return (
    <RouteSection
      title="Requirement Detail"
      description={`Requirement ${requirementId} within project ${projectId}.`}
    >
      <ResourceStateView
        state={state}
        emptyMessage="Requirement detail payload is empty."
        render={(data) => (
          <div className="grid">
            <JsonPanel title="Requirement" data={data} />
            <p>
              <Link to={`/planning/requirements/${encodeURIComponent(requirementId)}`}>
                Edit in Planning Workspace
              </Link>
            </p>
          </div>
        )}
      />
    </RouteSection>
  );
}

export function TasksPage() {
  return <TaskControlCenter />;
}

export function TaskDetailPage() {
  const params = useParams();
  const taskId = params.taskId ?? "";

  return <TaskControlCenter taskId={taskId} />;
}

function TaskControlCenter(props: { taskId?: string }) {
  const [refreshNonce, setRefreshNonce] = useState(0);
  const [statusFilter, setStatusFilter] = useState<"all" | QueueTaskStatus>("all");
  const [searchQuery, setSearchQuery] = useState("");
  const [selectedTaskId, setSelectedTaskId] = useState<string | null>(props.taskId ?? null);
  const [targetStatus, setTargetStatus] = useState<QueueTaskStatus>("ready");
  const [pendingTransition, setPendingTransition] = useState<string | null>(null);
  const [taskFeedback, setTaskFeedback] = useState<ControlFeedbackEntry[]>([]);
  const [transitionState, setTransitionState] = useState<
    { kind: "idle" } | { kind: "ok"; message: string } | { kind: "error"; message: string }
  >({ kind: "idle" });
  const [taskGate, setTaskGate] = useState<ActionGateConfig | null>(null);
  const [queuedGateTransition, setQueuedGateTransition] = useState<
    { taskId: string; status: QueueTaskStatus } | null
  >(null);

  const state = useApiResource(
    async () => {
      const detailPromise = props.taskId ? api.tasksById(props.taskId) : Promise.resolve(null);
      const [tasks, stats, selectedTask] = await Promise.all([
        api.tasksPrioritized(),
        api.tasksStats(),
        detailPromise,
      ]);

      const error = selectedTask
        ? firstApiError(tasks, stats, selectedTask)
        : firstApiError(tasks, stats);
      if (error) {
        return error;
      }

      const queue = [...tasks.data];
      if (
        selectedTask &&
        selectedTask.kind === "ok" &&
        !queue.some((task) => task.id === selectedTask.data.id)
      ) {
        queue.push(selectedTask.data as TaskSummary);
      }

      return {
        kind: "ok" as const,
        data: {
          tasks: queue,
          stats: stats.data,
        },
      };
    },
    [props.taskId, refreshNonce],
  );

  const queue = useMemo(() => {
    if (state.status !== "ready") {
      return [] as TaskSummary[];
    }

    return sortTaskQueue(state.data.tasks);
  }, [state]);

  useEffect(() => {
    if (props.taskId) {
      setSelectedTaskId(props.taskId);
      return;
    }

    if (queue.length === 0) {
      setSelectedTaskId(null);
      return;
    }

    setSelectedTaskId((current) => {
      if (current && queue.some((task) => task.id === current)) {
        return current;
      }
      return queue[0].id;
    });
  }, [props.taskId, queue]);

  const filteredQueue = useMemo(() => {
    const normalizedSearch = searchQuery.trim().toLowerCase();
    return queue.filter((task) => {
      const canonicalStatus = toQueueTaskStatus(task.status);
      if (statusFilter !== "all" && canonicalStatus !== statusFilter) {
        return false;
      }

      if (!normalizedSearch) {
        return true;
      }

      const haystack = `${task.id} ${taskTitle(task)} ${taskDescription(task)}`.toLowerCase();
      return haystack.includes(normalizedSearch);
    });
  }, [queue, statusFilter, searchQuery]);

  const selectedTask = useMemo(() => {
    if (selectedTaskId) {
      const matched = queue.find((task) => task.id === selectedTaskId);
      if (matched) {
        return matched;
      }
    }
    return filteredQueue[0] ?? queue[0] ?? null;
  }, [queue, filteredQueue, selectedTaskId]);

  useEffect(() => {
    if (!selectedTask) {
      return;
    }

    setTargetStatus(toQueueTaskStatus(selectedTask.status));
  }, [selectedTask?.id, selectedTask?.status]);

  const summary = useMemo(() => summarizeTaskQueue(queue, state.status === "ready" ? state.data.stats : null), [
    queue,
    state,
  ]);

  const appendTaskFeedback = (entry: Omit<ControlFeedbackEntry, "id">) => {
    setTaskFeedback((current) => {
      const next: ControlFeedbackEntry = {
        id: `${Date.now()}-${entry.targetId}-${entry.action}`,
        ...entry,
      };
      return [next, ...current].slice(0, CONTROL_FEEDBACK_LIMIT);
    });
  };

  const runTaskTransition = (taskId: string, status: QueueTaskStatus) => {
    if (pendingTransition) {
      return;
    }

    setPendingTransition(`${taskId}:${status}`);
    setTransitionState({ kind: "idle" });

    void api.taskSetStatus(taskId, { status }).then((result) => {
      if (result.kind === "error") {
        const message = formatApiError(result);
        setTransitionState({ kind: "error", message });
        appendTaskFeedback({
          action: "tasks.set_status",
          targetId: taskId,
          outcome: "error",
          timestamp: new Date().toISOString(),
          message,
          correlationId: result.correlationId,
        });
        setPendingTransition(null);
        return;
      }

      const nextStatus = toQueueTaskStatus(result.data.status);
      setTransitionState({
        kind: "ok",
        message: `${taskId} moved to ${formatStatusToken(nextStatus)}.`,
      });
      appendTaskFeedback({
        action: "tasks.set_status",
        targetId: taskId,
        outcome: "success",
        timestamp: new Date().toISOString(),
        message: `Status updated to ${nextStatus}.`,
      });
      setPendingTransition(null);
      setTaskGate(null);
      setQueuedGateTransition(null);
      setRefreshNonce((current) => current + 1);
    });
  };

  const requestTaskTransition = () => {
    if (!selectedTask) {
      return;
    }

    const currentStatus = toQueueTaskStatus(selectedTask.status);
    if (targetStatus === currentStatus) {
      setTransitionState({
        kind: "error",
        message: `${selectedTask.id} is already ${formatStatusToken(currentStatus)}.`,
      });
      return;
    }

    if (targetStatus === "cancelled" && ACTIVE_TASK_STATUSES.has(currentStatus)) {
      setQueuedGateTransition({ taskId: selectedTask.id, status: targetStatus });
      setTaskGate({
        actionKey: "tasks.set_status.cancelled",
        targetId: selectedTask.id,
        confirmationPhrase: `CANCEL ${selectedTask.id}`,
        impactSummary: `Cancelling ${selectedTask.id} marks this active task as terminal and removes it from active queue execution.`,
        submitLabel: "Confirm Task Cancellation",
      });
      return;
    }

    runTaskTransition(selectedTask.id, targetStatus);
  };

  const disableTransition =
    !selectedTask ||
    pendingTransition !== null ||
    targetStatus === toQueueTaskStatus(selectedTask.status);

  const transitionDisabledMessage = !selectedTask
    ? "Select a task to run transitions."
    : pendingTransition !== null
      ? "Task transition request in progress."
      : targetStatus === toQueueTaskStatus(selectedTask.status)
        ? "Choose a different status to apply."
        : null;

  const renderTaskSurface = (data: { tasks: TaskSummary[]; stats: TaskStatsPayload }) => (
    <div className="task-control-surface">
      <QueueSummaryStrip
        title="Task Queue Summary"
        values={[
          { label: "Total", value: summary.total.toString() },
          { label: "In Progress", value: summary.inProgress.toString() },
          { label: "Blocked", value: summary.blocked.toString() },
          { label: "Done", value: summary.done.toString() },
          { label: "Filtered", value: filteredQueue.length.toString() },
        ]}
      />

      <div className="task-control-grid">
        <section className="panel" aria-label="Task queue">
          <div className="panel-head">
            <h2>Queue</h2>
            <span className="muted-text">{data.tasks.length} tasks</span>
          </div>
          <div className="task-queue-toolbar">
            <label>
              Status filter
              <select
                value={statusFilter}
                onChange={(event) => setStatusFilter(event.target.value as "all" | QueueTaskStatus)}
              >
                <option value="all">All statuses</option>
                {TASK_STATUS_OPTIONS.map((status) => (
                  <option key={status} value={status}>
                    {formatStatusToken(status)}
                  </option>
                ))}
              </select>
            </label>
            <label>
              Search
              <input
                value={searchQuery}
                onChange={(event) => setSearchQuery(event.target.value)}
                placeholder="Search by id, title, description"
              />
            </label>
          </div>

          {filteredQueue.length === 0 ? (
            <EmptyState
              message={
                queue.length === 0
                  ? "No tasks returned."
                  : "No tasks match the active filters."
              }
            />
          ) : (
            <ul className="task-queue-list">
              {filteredQueue.map((task) => {
                const selected = selectedTask?.id === task.id;
                return (
                  <li key={task.id}>
                    <button
                      type="button"
                      className={`task-queue-row${selected ? " selected" : ""}`}
                      onClick={() => setSelectedTaskId(task.id)}
                      aria-pressed={selected}
                      aria-label={`Select task ${task.id}`}
                    >
                      <span className="task-id">{task.id}</span>
                      <span className="task-title">{taskTitle(task)}</span>
                      <span className="task-meta">
                        {formatStatusToken(toQueueTaskStatus(task.status))} | {formatPriority(task.priority)} |{" "}
                        {formatTimestamp(taskUpdatedAt(task))}
                      </span>
                    </button>
                  </li>
                );
              })}
            </ul>
          )}
        </section>

        <section className="panel" aria-label="Task controls">
          <div className="panel-head">
            <h2>Task Controls</h2>
            {selectedTask ? <code>{selectedTask.id}</code> : <span className="muted-text">No selection</span>}
          </div>
          {!selectedTask ? (
            <EmptyState message="Select a task from the queue to view controls." />
          ) : (
            <div className="task-detail-content">
              <p className="task-detail-title">{taskTitle(selectedTask)}</p>
              <p className="muted-text">{taskDescription(selectedTask)}</p>
              <p className="muted-text">
                Status: <strong>{formatStatusToken(toQueueTaskStatus(selectedTask.status))}</strong> | Priority:{" "}
                <strong>{formatPriority(selectedTask.priority)}</strong>
              </p>
              <p className="muted-text">
                Checklist: <strong>{checklistCompletedCount(selectedTask)}</strong> /{" "}
                <strong>{checklistTotalCount(selectedTask)}</strong> complete | Dependencies:{" "}
                <strong>{dependencyCount(selectedTask)}</strong>
              </p>

              <label>
                Next status
                <select
                  value={targetStatus}
                  onChange={(event) => setTargetStatus(event.target.value as QueueTaskStatus)}
                  disabled={pendingTransition !== null}
                >
                  {TASK_STATUS_OPTIONS.map((status) => (
                    <option key={status} value={status}>
                      {formatStatusToken(status)}
                    </option>
                  ))}
                </select>
              </label>

              <div className="panel-actions">
                <button type="button" onClick={requestTaskTransition} disabled={disableTransition}>
                  {pendingTransition ? "Updating..." : "Apply Status Transition"}
                </button>
                <Link className="action-link" to={`/tasks/${encodeURIComponent(selectedTask.id)}`}>
                  Open Task Route
                </Link>
              </div>

              {transitionDisabledMessage ? <p className="muted-text">{transitionDisabledMessage}</p> : null}
              {transitionState.kind === "ok" ? (
                <p role="status" aria-live="polite" className="status-box">
                  {transitionState.message}
                </p>
              ) : null}
              {transitionState.kind === "error" ? (
                <ErrorState
                  error={{
                    kind: "error",
                    code: "task_transition_failed",
                    message: transitionState.message,
                    exitCode: 1,
                  }}
                />
              ) : null}
            </div>
          )}
        </section>
      </div>

      <ControlFeedbackLog title="Task Action Feedback" entries={taskFeedback} emptyMessage="No task actions yet." />
      <DiagnosticsPanel title="Task Diagnostics" actionPrefixes={["tasks."]} />
      <ActionGateDialog
        gate={taskGate}
        pending={pendingTransition !== null}
        onClose={() => {
          if (pendingTransition) {
            return;
          }
          setTaskGate(null);
          setQueuedGateTransition(null);
        }}
        onConfirm={() => {
          if (!queuedGateTransition) {
            return;
          }
          runTaskTransition(queuedGateTransition.taskId, queuedGateTransition.status);
        }}
      />
    </div>
  );

  return (
    <RouteSection
      title={props.taskId ? "Task Detail" : "Tasks"}
      description={
        props.taskId
          ? `Control center for task ${props.taskId}.`
          : "Queue-first task control center with deterministic transitions."
      }
    >
      <ResourceStateView
        state={state}
        emptyMessage="No task records returned."
        render={renderTaskSurface}
      />
    </RouteSection>
  );
}

export function WorkflowsPage() {
  const [refreshNonce, setRefreshNonce] = useState(0);
  const [runTaskId, setRunTaskId] = useState("");
  const [runPipelineId, setRunPipelineId] = useState("");
  const [pendingAction, setPendingAction] = useState<string | null>(null);
  const [actionState, setActionState] = useState<
    { kind: "idle" } | { kind: "ok"; message: string } | { kind: "error"; message: string }
  >({ kind: "idle" });
  const [workflowFeedback, setWorkflowFeedback] = useState<ControlFeedbackEntry[]>([]);
  const [workflowGate, setWorkflowGate] = useState<ActionGateConfig | null>(null);
  const [queuedCancelWorkflowId, setQueuedCancelWorkflowId] = useState<string | null>(null);

  const state = useApiResource(
    async () => api.workflowsList(),
    [refreshNonce],
    {
      isEmpty: (data) => Array.isArray(data) && data.length === 0,
    },
  );

  const appendWorkflowFeedback = (entry: Omit<ControlFeedbackEntry, "id">) => {
    setWorkflowFeedback((current) => {
      const next: ControlFeedbackEntry = {
        id: `${Date.now()}-${entry.targetId}-${entry.action}`,
        ...entry,
      };
      return [next, ...current].slice(0, CONTROL_FEEDBACK_LIMIT);
    });
  };

  const runWorkflow = (taskId: string, pipelineId: string | null) => {
    if (pendingAction) {
      return;
    }

    setPendingAction("run");
    setActionState({ kind: "idle" });
    void api
      .workflowRun({
        task_id: taskId,
        ...(pipelineId ? { pipeline_id: pipelineId } : {}),
      })
      .then((result) => {
        if (result.kind === "error") {
          const message = formatApiError(result);
          setActionState({ kind: "error", message });
          appendWorkflowFeedback({
            action: "workflows.run",
            targetId: taskId,
            outcome: "error",
            timestamp: new Date().toISOString(),
            message,
            correlationId: result.correlationId,
          });
          setPendingAction(null);
          return;
        }

        setActionState({
          kind: "ok",
          message: `Workflow ${result.data.id} started for task ${result.data.task_id ?? taskId}.`,
        });
        appendWorkflowFeedback({
          action: "workflows.run",
          targetId: result.data.id,
          outcome: "success",
          timestamp: new Date().toISOString(),
          message: "Workflow run started.",
        });
        setPendingAction(null);
        setRefreshNonce((current) => current + 1);
      });
  };

  const runWorkflowAction = (workflowId: string, action: "pause" | "resume" | "cancel") => {
    if (pendingAction) {
      return;
    }

    setPendingAction(`${action}:${workflowId}`);
    setActionState({ kind: "idle" });
    const request =
      action === "pause"
        ? api.workflowPause(workflowId)
        : action === "resume"
          ? api.workflowResume(workflowId)
          : api.workflowCancel(workflowId);

    void request.then((result) => {
      if (result.kind === "error") {
        const message = formatApiError(result);
        setActionState({ kind: "error", message });
        appendWorkflowFeedback({
          action: `workflows.${action}`,
          targetId: workflowId,
          outcome: "error",
          timestamp: new Date().toISOString(),
          message,
          correlationId: result.correlationId,
        });
        setPendingAction(null);
        return;
      }

      setActionState({
        kind: "ok",
        message: `Workflow ${workflowId} ${action} request completed.`,
      });
      appendWorkflowFeedback({
        action: `workflows.${action}`,
        targetId: workflowId,
        outcome: "success",
        timestamp: new Date().toISOString(),
        message: `Workflow status is ${formatStatusToken(result.data.status)}.`,
      });
      setPendingAction(null);
      setWorkflowGate(null);
      setQueuedCancelWorkflowId(null);
      setRefreshNonce((current) => current + 1);
    });
  };

  const onRunSubmit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const taskId = runTaskId.trim();
    if (!taskId) {
      setActionState({
        kind: "error",
        message: "Task ID is required to run a workflow.",
      });
      return;
    }
    runWorkflow(taskId, normalizeOptionalString(runPipelineId));
  };

  const renderWorkflowList = (workflows: WorkflowSummary[]) => {
    const summary = summarizeWorkflowList(workflows);

    return (
      <div className="workflow-control-surface">
        <QueueSummaryStrip
          title="Workflow Summary"
          values={[
            { label: "Total", value: summary.total.toString() },
            { label: "Running", value: summary.running.toString() },
            { label: "Paused", value: summary.paused.toString() },
            { label: "Pending", value: summary.pending.toString() },
            { label: "Terminal", value: summary.terminal.toString() },
          ]}
        />

        <div className="grid two">
          <form className="panel workflow-run-form" onSubmit={onRunSubmit}>
            <h2>Run Workflow</h2>
            <label>
              Task ID
              <input
                required
                value={runTaskId}
                onChange={(event) => setRunTaskId(event.target.value)}
                placeholder="TASK-014"
              />
            </label>
            <label>
              Pipeline ID (optional)
              <input
                value={runPipelineId}
                onChange={(event) => setRunPipelineId(event.target.value)}
                placeholder="default"
              />
            </label>
            <div className="panel-actions">
              <button type="submit" disabled={pendingAction !== null}>
                {pendingAction === "run" ? "Starting..." : "Run Workflow"}
              </button>
            </div>
          </form>

          <section className="panel" aria-label="Workflow list">
            <h2>Workflow Queue</h2>
            <ul className="workflow-list">
              {workflows.map((workflow) => {
                const availability = workflowAvailability(workflow.status);
                return (
                  <li key={workflow.id} className="workflow-card">
                    <div className="workflow-card-header">
                      <div>
                        <p className="task-detail-title">{workflow.id}</p>
                        <p className="muted-text">
                          Task: <code>{workflow.task_id ?? "unknown"}</code>
                        </p>
                      </div>
                      <span className={`status-chip status-${toWorkflowStatus(workflow.status)}`}>
                        {formatStatusToken(toWorkflowStatus(workflow.status))}
                      </span>
                    </div>
                    <p className="muted-text">
                      Phase: <code>{workflow.current_phase ?? "none"}</code>
                    </p>
                    <div className="panel-actions">
                      <button
                        type="button"
                        disabled={pendingAction !== null || !availability.pause}
                        onClick={() => runWorkflowAction(workflow.id, "pause")}
                        aria-label={`Pause workflow ${workflow.id}`}
                      >
                        Pause
                      </button>
                      <button
                        type="button"
                        disabled={pendingAction !== null || !availability.resume}
                        onClick={() => runWorkflowAction(workflow.id, "resume")}
                        aria-label={`Resume workflow ${workflow.id}`}
                      >
                        Resume
                      </button>
                      <button
                        type="button"
                        className="danger-button"
                        disabled={pendingAction !== null || !availability.cancel}
                        onClick={() => {
                          setQueuedCancelWorkflowId(workflow.id);
                          setWorkflowGate({
                            actionKey: "workflows.cancel",
                            targetId: workflow.id,
                            confirmationPhrase: `CANCEL ${workflow.id}`,
                            impactSummary: `Cancelling ${workflow.id} can interrupt active phase execution and stops further progression.`,
                            submitLabel: "Confirm Workflow Cancellation",
                          });
                        }}
                        aria-label={`Cancel workflow ${workflow.id}`}
                      >
                        Cancel
                      </button>
                      <Link className="action-link" to={`/workflows/${encodeURIComponent(workflow.id)}`}>
                        Open
                      </Link>
                    </div>
                  </li>
                );
              })}
            </ul>
          </section>
        </div>

        {actionState.kind === "ok" ? (
          <p role="status" aria-live="polite" className="status-box">
            {actionState.message}
          </p>
        ) : null}
        {actionState.kind === "error" ? (
          <ErrorState
            error={{
              kind: "error",
              code: "workflow_action_failed",
              message: actionState.message,
              exitCode: 1,
            }}
          />
        ) : null}

        <ControlFeedbackLog
          title="Workflow Action Feedback"
          entries={workflowFeedback}
          emptyMessage="No workflow actions yet."
        />
        <DiagnosticsPanel title="Workflow Diagnostics" actionPrefixes={["workflows."]} />
        <ActionGateDialog
          gate={workflowGate}
          pending={pendingAction !== null}
          onClose={() => {
            if (pendingAction) {
              return;
            }
            setWorkflowGate(null);
            setQueuedCancelWorkflowId(null);
          }}
          onConfirm={() => {
            if (!queuedCancelWorkflowId) {
              return;
            }
            runWorkflowAction(queuedCancelWorkflowId, "cancel");
          }}
        />
      </div>
    );
  };

  return (
    <RouteSection title="Workflows" description="Workflow run controls and lifecycle queue.">
      <ResourceStateView
        state={state}
        emptyMessage="No workflow records returned."
        render={renderWorkflowList}
      />
    </RouteSection>
  );
}

export function WorkflowDetailPage() {
  const params = useParams();
  const workflowId = params.workflowId ?? "";
  const [refreshNonce, setRefreshNonce] = useState(0);
  const [runTaskId, setRunTaskId] = useState("");
  const [runPipelineId, setRunPipelineId] = useState("");
  const [pendingAction, setPendingAction] = useState<string | null>(null);
  const [actionState, setActionState] = useState<
    { kind: "idle" } | { kind: "ok"; message: string } | { kind: "error"; message: string }
  >({ kind: "idle" });
  const [workflowFeedback, setWorkflowFeedback] = useState<ControlFeedbackEntry[]>([]);
  const [workflowGate, setWorkflowGate] = useState<ActionGateConfig | null>(null);
  const [queuedCancelWorkflowId, setQueuedCancelWorkflowId] = useState<string | null>(null);

  const state = useApiResource(
    async () => {
      const [workflow, decisions, checkpoints] = await Promise.all([
        api.workflowsById(workflowId),
        api.workflowDecisions(workflowId),
        api.workflowCheckpoints(workflowId),
      ]);
      const error = firstApiError(workflow, decisions, checkpoints);
      if (error) {
        return error;
      }

      return {
        kind: "ok" as const,
        data: {
          workflow: workflow.data,
          decisions: decisions.data,
          checkpoints: checkpoints.data,
        },
      };
    },
    [workflowId, refreshNonce],
  );

  useEffect(() => {
    if (state.status !== "ready") {
      return;
    }

    setRunTaskId((current) =>
      current.length > 0 ? current : (state.data.workflow.task_id ?? ""),
    );
    setRunPipelineId((current) =>
      current.length > 0 ? current : (state.data.workflow.pipeline_id ?? ""),
    );
  }, [state]);

  const appendWorkflowFeedback = (entry: Omit<ControlFeedbackEntry, "id">) => {
    setWorkflowFeedback((current) => {
      const next: ControlFeedbackEntry = {
        id: `${Date.now()}-${entry.targetId}-${entry.action}`,
        ...entry,
      };
      return [next, ...current].slice(0, CONTROL_FEEDBACK_LIMIT);
    });
  };

  const runWorkflow = (taskId: string, pipelineId: string | null) => {
    if (pendingAction) {
      return;
    }

    setPendingAction("run");
    setActionState({ kind: "idle" });
    void api
      .workflowRun({
        task_id: taskId,
        ...(pipelineId ? { pipeline_id: pipelineId } : {}),
      })
      .then((result) => {
        if (result.kind === "error") {
          const message = formatApiError(result);
          setActionState({ kind: "error", message });
          appendWorkflowFeedback({
            action: "workflows.run",
            targetId: taskId,
            outcome: "error",
            timestamp: new Date().toISOString(),
            message,
            correlationId: result.correlationId,
          });
          setPendingAction(null);
          return;
        }

        setActionState({
          kind: "ok",
          message: `Workflow ${result.data.id} started for task ${result.data.task_id ?? taskId}.`,
        });
        appendWorkflowFeedback({
          action: "workflows.run",
          targetId: result.data.id,
          outcome: "success",
          timestamp: new Date().toISOString(),
          message: "Workflow run started.",
        });
        setPendingAction(null);
        setRefreshNonce((current) => current + 1);
      });
  };

  const runWorkflowAction = (action: "pause" | "resume" | "cancel") => {
    if (pendingAction) {
      return;
    }

    setPendingAction(`${action}:${workflowId}`);
    setActionState({ kind: "idle" });
    const request =
      action === "pause"
        ? api.workflowPause(workflowId)
        : action === "resume"
          ? api.workflowResume(workflowId)
          : api.workflowCancel(workflowId);

    void request.then((result) => {
      if (result.kind === "error") {
        const message = formatApiError(result);
        setActionState({ kind: "error", message });
        appendWorkflowFeedback({
          action: `workflows.${action}`,
          targetId: workflowId,
          outcome: "error",
          timestamp: new Date().toISOString(),
          message,
          correlationId: result.correlationId,
        });
        setPendingAction(null);
        return;
      }

      setActionState({
        kind: "ok",
        message: `Workflow ${workflowId} ${action} request completed.`,
      });
      appendWorkflowFeedback({
        action: `workflows.${action}`,
        targetId: workflowId,
        outcome: "success",
        timestamp: new Date().toISOString(),
        message: `Workflow status is ${formatStatusToken(result.data.status)}.`,
      });
      setPendingAction(null);
      setWorkflowGate(null);
      setQueuedCancelWorkflowId(null);
      setRefreshNonce((current) => current + 1);
    });
  };

  const onRunSubmit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const taskId = runTaskId.trim();
    if (!taskId) {
      setActionState({
        kind: "error",
        message: "Task ID is required to run a workflow.",
      });
      return;
    }
    runWorkflow(taskId, normalizeOptionalString(runPipelineId));
  };

  return (
    <RouteSection title="Workflow Detail" description={`Workflow ${workflowId}.`}>
      <ResourceStateView
        state={state}
        emptyMessage="Workflow detail payload is empty."
        render={(data) => {
          const availability = workflowAvailability(data.workflow.status);
          const timelineEntries = buildTimelineEntries(data.checkpoints, data.decisions);
          return (
            <div className="workflow-control-surface">
              <QueueSummaryStrip
                title="Workflow Status"
                values={[
                  { label: "Workflow", value: data.workflow.id },
                  { label: "Status", value: formatStatusToken(toWorkflowStatus(data.workflow.status)) },
                  { label: "Current Phase", value: data.workflow.current_phase ?? "none" },
                  { label: "Task", value: data.workflow.task_id ?? "unknown" },
                ]}
              />

              <div className="grid two">
                <form className="panel workflow-run-form" onSubmit={onRunSubmit}>
                  <h2>Workflow Controls</h2>
                  <label>
                    Task ID
                    <input
                      required
                      value={runTaskId}
                      onChange={(event) => setRunTaskId(event.target.value)}
                    />
                  </label>
                  <label>
                    Pipeline ID (optional)
                    <input
                      value={runPipelineId}
                      onChange={(event) => setRunPipelineId(event.target.value)}
                    />
                  </label>

                  <div className="panel-actions">
                    <button type="submit" disabled={pendingAction !== null}>
                      {pendingAction === "run" ? "Starting..." : "Run"}
                    </button>
                    <button
                      type="button"
                      onClick={() => runWorkflowAction("pause")}
                      disabled={pendingAction !== null || !availability.pause}
                    >
                      Pause
                    </button>
                    <button
                      type="button"
                      onClick={() => runWorkflowAction("resume")}
                      disabled={pendingAction !== null || !availability.resume}
                    >
                      Resume
                    </button>
                    <button
                      type="button"
                      className="danger-button"
                      onClick={() => {
                        setQueuedCancelWorkflowId(data.workflow.id);
                        setWorkflowGate({
                          actionKey: "workflows.cancel",
                          targetId: data.workflow.id,
                          confirmationPhrase: `CANCEL ${data.workflow.id}`,
                          impactSummary: `Cancelling ${data.workflow.id} can interrupt active phase execution and stops further progression.`,
                          submitLabel: "Confirm Workflow Cancellation",
                        });
                      }}
                      disabled={pendingAction !== null || !availability.cancel}
                    >
                      Cancel
                    </button>
                  </div>
                  <p className="muted-text">
                    Started: {formatTimestamp(data.workflow.started_at)} | Completed:{" "}
                    {formatTimestamp(data.workflow.completed_at)}
                  </p>
                </form>

                <section className="panel" aria-label="Phase timeline">
                  <h2>Phase Timeline</h2>
                  {timelineEntries.length === 0 ? (
                    <EmptyState message="No checkpoints or decisions recorded yet." />
                  ) : (
                    <ol className="workflow-timeline">
                      {timelineEntries.map((entry) => (
                        <li key={entry.key} className="workflow-timeline-entry">
                          <div className="workflow-timeline-head">
                            <strong>{entry.heading}</strong>
                            <span className="muted-text">{entry.timestampLabel}</span>
                          </div>
                          <p className="muted-text">{entry.detail}</p>
                          {entry.checkpointNumber !== undefined ? (
                            <Link
                              className="action-link"
                              to={`/workflows/${encodeURIComponent(workflowId)}/checkpoints/${entry.checkpointNumber}`}
                            >
                              Open checkpoint {entry.checkpointNumber}
                            </Link>
                          ) : null}
                        </li>
                      ))}
                    </ol>
                  )}
                </section>
              </div>

              {actionState.kind === "ok" ? (
                <p role="status" aria-live="polite" className="status-box">
                  {actionState.message}
                </p>
              ) : null}
              {actionState.kind === "error" ? (
                <ErrorState
                  error={{
                    kind: "error",
                    code: "workflow_action_failed",
                    message: actionState.message,
                    exitCode: 1,
                  }}
                />
              ) : null}

              <ControlFeedbackLog
                title="Workflow Action Feedback"
                entries={workflowFeedback}
                emptyMessage="No workflow actions yet."
              />
              <DiagnosticsPanel title="Workflow Diagnostics" actionPrefixes={["workflows."]} />
              <ActionGateDialog
                gate={workflowGate}
                pending={pendingAction !== null}
                onClose={() => {
                  if (pendingAction) {
                    return;
                  }
                  setWorkflowGate(null);
                  setQueuedCancelWorkflowId(null);
                }}
                onConfirm={() => {
                  if (!queuedCancelWorkflowId) {
                    return;
                  }
                  runWorkflowAction("cancel");
                }}
              />
            </div>
          );
        }}
      />
    </RouteSection>
  );
}

export function WorkflowCheckpointPage() {
  const params = useParams();
  const workflowId = params.workflowId ?? "";
  const checkpoint = params.checkpoint ?? "";

  const state = useApiResource(
    async () => api.workflowCheckpointById(workflowId, checkpoint),
    [workflowId, checkpoint],
  );

  return (
    <RouteSection
      title="Checkpoint Detail"
      description={`Checkpoint ${checkpoint} for workflow ${workflowId}.`}
    >
      <ResourceStateView
        state={state}
        emptyMessage="Checkpoint payload is empty."
        render={(data) => <JsonPanel title="Checkpoint" data={data} />}
      />
    </RouteSection>
  );
}

function QueueSummaryStrip(props: {
  title: string;
  values: Array<{ label: string; value: string }>;
}) {
  return (
    <section className="queue-summary" aria-label={props.title}>
      <h2>{props.title}</h2>
      <ul className="queue-summary-list">
        {props.values.map((item) => (
          <li key={item.label}>
            <span>{item.label}</span>
            <strong>{item.value}</strong>
          </li>
        ))}
      </ul>
    </section>
  );
}

function ControlFeedbackLog(props: {
  title: string;
  entries: ControlFeedbackEntry[];
  emptyMessage: string;
}) {
  return (
    <section className="panel" aria-label={props.title}>
      <h2>{props.title}</h2>
      {props.entries.length === 0 ? (
        <EmptyState message={props.emptyMessage} />
      ) : (
        <ul className="feedback-list">
          {props.entries.map((entry) => (
            <li key={entry.id} className={`feedback-item feedback-${entry.outcome}`}>
              <div className="feedback-head">
                <strong>{entry.action}</strong>
                <span className="muted-text">{formatTimestamp(entry.timestamp)}</span>
              </div>
              <p className="muted-text">
                Target: <code>{entry.targetId}</code> | Outcome: {entry.outcome}
              </p>
              <p>{entry.message}</p>
              {entry.correlationId ? (
                <p className="muted-text">
                  Correlation: <code>{entry.correlationId}</code>
                </p>
              ) : null}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

type TimelineEntry = {
  key: string;
  heading: string;
  detail: string;
  timestampLabel: string;
  order: number;
  timestamp: number;
  checkpointNumber?: number;
};

function buildTimelineEntries(
  checkpoints: WorkflowCheckpoint[],
  decisions: WorkflowDecision[],
): TimelineEntry[] {
  const entries: TimelineEntry[] = [];

  checkpoints.forEach((checkpoint, index) => {
    const record = asRecord(checkpoint);
    const number = readNumber(record, ["number", "checkpoint", "order"]);
    const timestamp = readString(record, ["timestamp", "created_at"]);
    const reason = readString(record, ["reason"]) ?? "manual";
    const status = readString(record, ["status"]) ?? "unknown";
    const machineState = readString(record, ["machine_state"]);

    entries.push({
      key: `checkpoint-${number ?? "na"}-${timestamp ?? "na"}-${index}`,
      heading: `Checkpoint ${number ?? "?"} · ${formatStatusToken(status)}`,
      detail: [machineState ? `Machine ${machineState}` : null, `Reason ${reason}`]
        .filter((value): value is string => value !== null)
        .join(" | "),
      timestampLabel: formatTimestamp(timestamp),
      order: number ?? Number.MAX_SAFE_INTEGER - 1,
      timestamp: toEpoch(timestamp),
      ...(number !== null ? { checkpointNumber: number } : {}),
    });
  });

  decisions.forEach((decision, index) => {
    const record = asRecord(decision);
    const phase = readString(record, ["phase_id", "phase"]) ?? "unknown phase";
    const decisionText = readString(record, ["decision"]) ?? "decision";
    const timestamp = readString(record, ["timestamp", "created_at"]);
    const risk = readString(record, ["risk"]);
    const source = readString(record, ["source"]);
    const reason = readString(record, ["reason"]);

    entries.push({
      key: `decision-${phase}-${timestamp ?? "na"}-${index}`,
      heading: `Decision · ${phase} · ${decisionText}`,
      detail: [risk ? `Risk ${risk}` : null, source ? `Source ${source}` : null, reason ?? null]
        .filter((value): value is string => value !== null)
        .join(" | "),
      timestampLabel: formatTimestamp(timestamp),
      order:
        readNumber(record, [
          "checkpoint_order",
          "checkpoint",
          "order",
          "phase_index",
          "current_phase_index",
        ]) ?? Number.MAX_SAFE_INTEGER,
      timestamp: toEpoch(timestamp),
    });
  });

  return entries.sort((left, right) => {
    if (left.order !== right.order) {
      return left.order - right.order;
    }
    if (left.timestamp !== right.timestamp) {
      return left.timestamp - right.timestamp;
    }
    return left.key.localeCompare(right.key);
  });
}

function summarizeTaskQueue(tasks: TaskSummary[], stats: TaskStatsPayload | null) {
  const fallback = {
    total: tasks.length,
    inProgress: tasks.filter((task) => toQueueTaskStatus(task.status) === "in-progress").length,
    blocked: tasks.filter((task) => {
      const status = toQueueTaskStatus(task.status);
      return status === "blocked" || status === "on-hold";
    }).length,
    done: tasks.filter((task) => toQueueTaskStatus(task.status) === "done").length,
  };

  if (!stats) {
    return fallback;
  }

  return {
    total: stats.total ?? fallback.total,
    inProgress: stats.in_progress ?? fallback.inProgress,
    blocked: stats.blocked ?? fallback.blocked,
    done: stats.completed ?? fallback.done,
  };
}

function summarizeWorkflowList(workflows: WorkflowSummary[]) {
  let running = 0;
  let paused = 0;
  let pending = 0;
  let terminal = 0;

  workflows.forEach((workflow) => {
    const status = toWorkflowStatus(workflow.status);
    if (status === "running") {
      running += 1;
    } else if (status === "paused") {
      paused += 1;
    } else if (status === "pending") {
      pending += 1;
    } else if (status === "completed" || status === "failed" || status === "cancelled") {
      terminal += 1;
    }
  });

  return {
    total: workflows.length,
    running,
    paused,
    pending,
    terminal,
  };
}

function workflowAvailability(status: WorkflowStatusValue | undefined) {
  const normalized = toWorkflowStatus(status);
  return {
    pause: normalized === "running",
    resume: normalized === "paused",
    cancel: normalized === "running" || normalized === "paused",
  };
}

function taskTitle(task: TaskSummary | TaskDetail): string {
  return (typeof task.title === "string" && task.title.trim().length > 0 ? task.title : "Untitled task").trim();
}

function taskDescription(task: TaskSummary | TaskDetail): string {
  return typeof task.description === "string" && task.description.trim().length > 0
    ? task.description
    : "No description provided.";
}

function taskUpdatedAt(task: TaskSummary | TaskDetail): string | null {
  if (typeof task.updated_at === "string" && task.updated_at.trim().length > 0) {
    return task.updated_at;
  }

  const metadata = asRecord(task.metadata);
  const metadataUpdatedAt = readString(metadata, ["updated_at"]);
  return metadataUpdatedAt;
}

function checklistTotalCount(task: TaskSummary | TaskDetail): number {
  return Array.isArray(task.checklist) ? task.checklist.length : 0;
}

function checklistCompletedCount(task: TaskSummary | TaskDetail): number {
  if (!Array.isArray(task.checklist)) {
    return 0;
  }

  return task.checklist.filter((entry) => {
    const record = asRecord(entry);
    return record["completed"] === true;
  }).length;
}

function dependencyCount(task: TaskSummary | TaskDetail): number {
  return Array.isArray(task.dependencies) ? task.dependencies.length : 0;
}

function sortTaskQueue(tasks: TaskSummary[]): TaskSummary[] {
  return [...tasks].sort((left, right) => {
    const leftPriority = priorityRank(left.priority);
    const rightPriority = priorityRank(right.priority);
    if (leftPriority !== rightPriority) {
      return leftPriority - rightPriority;
    }

    const leftUpdatedAt = toEpoch(taskUpdatedAt(left));
    const rightUpdatedAt = toEpoch(taskUpdatedAt(right));
    if (leftUpdatedAt !== rightUpdatedAt) {
      return rightUpdatedAt - leftUpdatedAt;
    }

    return left.id.localeCompare(right.id);
  });
}

function toQueueTaskStatus(status: TaskStatusValue | undefined): QueueTaskStatus {
  if (status && TASK_STATUS_OPTIONS.includes(status as QueueTaskStatus)) {
    return status as QueueTaskStatus;
  }
  return "backlog";
}

function toWorkflowStatus(status: WorkflowStatusValue | undefined): WorkflowStatusValue {
  return status ?? "unknown";
}

function priorityRank(priority: PriorityValue | undefined): number {
  if (priority && priority !== "unknown") {
    return PRIORITY_ORDER[priority];
  }
  return Number.MAX_SAFE_INTEGER;
}

function formatPriority(priority: PriorityValue | undefined): string {
  if (!priority || priority === "unknown") {
    return "Unknown";
  }
  return formatStatusToken(priority);
}

function formatStatusToken(status: string | undefined): string {
  if (!status || status.trim().length === 0) {
    return "Unknown";
  }
  return status
    .replace(/[_-]/g, " ")
    .replace(/\s+/g, " ")
    .trim()
    .replace(/\b\w/g, (char) => char.toUpperCase());
}

function formatTimestamp(value: string | null | undefined): string {
  if (!value || value.trim().length === 0) {
    return "Unknown time";
  }
  return value;
}

function formatApiError(error: ApiError): string {
  const correlation = error.correlationId ? ` (correlation ${error.correlationId})` : "";
  return `${error.code}: ${error.message}${correlation}`;
}

function normalizeOptionalString(value: string): string | null {
  const trimmed = value.trim();
  return trimmed.length === 0 ? null : trimmed;
}

function toEpoch(value: string | null | undefined): number {
  if (!value) {
    return Number.MAX_SAFE_INTEGER;
  }

  const parsed = Date.parse(value);
  if (Number.isNaN(parsed)) {
    return Number.MAX_SAFE_INTEGER;
  }
  return parsed;
}

function asRecord(value: unknown): Record<string, unknown> {
  if (typeof value === "object" && value !== null) {
    return value as Record<string, unknown>;
  }
  return {};
}

function readString(record: Record<string, unknown>, keys: string[]): string | null {
  for (const key of keys) {
    const value = record[key];
    if (typeof value === "string" && value.trim().length > 0) {
      return value;
    }
  }
  return null;
}

function readNumber(record: Record<string, unknown>, keys: string[]): number | null {
  for (const key of keys) {
    const value = record[key];
    if (typeof value === "number" && Number.isFinite(value)) {
      return value;
    }
    if (typeof value === "string" && value.trim().length > 0) {
      const parsed = Number(value);
      if (Number.isFinite(parsed)) {
        return parsed;
      }
    }
  }
  return null;
}

export { matchesConfirmationPhrase };

export function EventsPage() {
  const { connectionState, events } = useDaemonEvents();

  const mostRecentEvents = useMemo(() => {
    return [...events].reverse().slice(0, 25);
  }, [events]);

  return (
    <RouteSection
      title="Events"
      description="Live daemon event stream with reconnect and Last-Event-ID resume."
    >
      <p>
        Stream state: <code>{connectionState}</code>
      </p>
      <div aria-live="polite" aria-atomic="false" className="panel">
        <h2>Latest Events</h2>
        {mostRecentEvents.length === 0 ? (
          <EmptyState message="No daemon events have been received yet." />
        ) : (
          <pre>{JSON.stringify(mostRecentEvents, null, 2)}</pre>
        )}
      </div>
    </RouteSection>
  );
}

export function ReviewHandoffPage() {
  const [runId, setRunId] = useState("");
  const [targetRole, setTargetRole] = useState("em");
  const [question, setQuestion] = useState("");
  const [contextJson, setContextJson] = useState("{}");
  const [submitState, setSubmitState] = useState<
    { kind: "idle" } | { kind: "ok"; data: unknown } | { kind: "error"; error: ApiError }
  >({ kind: "idle" });

  const onSubmit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();

    let contextPayload: unknown;
    try {
      contextPayload = JSON.parse(contextJson || "{}");
    } catch {
      setSubmitState({
        kind: "error",
        error: {
          kind: "error",
          code: "invalid_context",
          message: "Context must be valid JSON.",
          exitCode: 2,
        },
      });
      return;
    }

    if (!isJsonValue(contextPayload)) {
      setSubmitState({
        kind: "error",
        error: {
          kind: "error",
          code: "invalid_context",
          message: "Context must be a valid JSON value.",
          exitCode: 2,
        },
      });
      return;
    }

    void api
      .reviewHandoff({
        run_id: runId,
        target_role: targetRole,
        question,
        context: contextPayload,
      })
      .then((result) => {
        if (result.kind === "error") {
          setSubmitState({ kind: "error", error: result });
          return;
        }

        setSubmitState({ kind: "ok", data: result.data });
      });
  };

  return (
    <RouteSection title="Review Handoff" description="Submit review handoff payloads to AO.">
      <form className="panel grid" onSubmit={onSubmit}>
        <label>
          Run ID
          <input value={runId} onChange={(event) => setRunId(event.target.value)} />
        </label>

        <label>
          Target Role
          <select value={targetRole} onChange={(event) => setTargetRole(event.target.value)}>
            <option value="em">em</option>
            <option value="reviewer">reviewer</option>
            <option value="qa">qa</option>
          </select>
        </label>

        <label>
          Question
          <textarea
            rows={3}
            required
            value={question}
            onChange={(event) => setQuestion(event.target.value)}
          />
        </label>

        <label>
          Context JSON
          <textarea
            rows={4}
            value={contextJson}
            onChange={(event) => setContextJson(event.target.value)}
          />
        </label>

        <div className="panel-actions">
          <button type="submit">Submit Handoff</button>
        </div>
      </form>

      {submitState.kind === "error" ? <ErrorState error={submitState.error} /> : null}
      {submitState.kind === "ok" ? <JsonPanel title="Response" data={submitState.data} /> : null}
      <DiagnosticsPanel
        title="Review Handoff Diagnostics"
        actionPrefixes={["reviews.handoff"]}
      />
    </RouteSection>
  );
}

export function NotFoundPage() {
  return (
    <RouteSection title="Not Found" description="The requested route does not exist.">
      <ErrorState
        error={{
          kind: "error",
          code: "not_found",
          message: "Unknown route. Return to the dashboard.",
          exitCode: 3,
        }}
      />
      <p>
        <Link to="/dashboard">Go to dashboard</Link>
      </p>
    </RouteSection>
  );
}

function RouteSection(props: {
  title: string;
  description: string;
  children: ReactNode;
}) {
  return (
    <section className="panel" aria-label={props.title}>
      <h1>{props.title}</h1>
      <p>{props.description}</p>
      {props.children}
    </section>
  );
}

function ResourceStateView<TData>(props: {
  state: ResourceState<TData>;
  emptyMessage: string;
  render: (data: TData) => ReactNode;
}) {
  if (props.state.status === "loading") {
    return <LoadingState message="Loading data..." />;
  }

  if (props.state.status === "error") {
    return <ErrorState error={props.state.error} />;
  }

  if (props.state.status === "empty") {
    return <EmptyState message={props.emptyMessage} />;
  }

  return <>{props.render(props.state.data)}</>;
}

function LoadingState(props: { message: string }) {
  return <div className="loading-box">{props.message}</div>;
}

function EmptyState(props: { message: string }) {
  return <div className="empty-box">{props.message}</div>;
}

function ErrorState(props: { error: ApiError }) {
  return (
    <div className="error-box" role="alert">
      <strong>Error:</strong> {props.error.code}
      <div>{props.error.message}</div>
      <div>exit code {props.error.exitCode}</div>
    </div>
  );
}

function JsonPanel(props: { title: string; data: unknown }) {
  return (
    <div className="panel">
      <h2>{props.title}</h2>
      <pre>{JSON.stringify(props.data, null, 2)}</pre>
    </div>
  );
}

function isJsonValue(value: unknown): value is RequestJsonValue {
  if (
    value === null ||
    typeof value === "string" ||
    typeof value === "number" ||
    typeof value === "boolean"
  ) {
    return true;
  }

  if (Array.isArray(value)) {
    return value.every((item) => isJsonValue(item));
  }

  if (typeof value === "object" && value !== null) {
    return Object.values(value).every((item) => isJsonValue(item));
  }

  return false;
}
