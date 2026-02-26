import { FormEvent, ReactNode, useEffect, useMemo, useRef, useState } from "react";
import { Link, useParams } from "react-router-dom";

import { useProjectContext } from "./project-context";
import { DiagnosticsPanel } from "./diagnostics-panel";
import { api, firstApiError, RequestJsonValue } from "../lib/api/client";
import { ApiError } from "../lib/api/envelope";
import { ResourceState, useApiResource } from "../lib/api/use-api-resource";
import { useDaemonEvents } from "../lib/events/use-daemon-events";
import { generateCorrelationId } from "../lib/telemetry";

type DaemonAction = "start" | "pause" | "resume" | "stop" | "clear";
type DaemonActionName =
  | "daemon.start"
  | "daemon.pause"
  | "daemon.resume"
  | "daemon.stop"
  | "daemon.clear_logs";
type DaemonActionRisk = "low" | "medium" | "high";

type DaemonActionConfig = {
  actionName: DaemonActionName;
  label: string;
  method: "POST" | "DELETE";
  path: string;
  risk: DaemonActionRisk;
  destructive: boolean;
  impact: string;
  plannedEffects: string[];
  irreversibleConsequence?: string;
  rollbackGuidance?: string;
  confirmationPhrase?: string;
};

type ActionFeedbackRecord = {
  id: string;
  action: DaemonActionName;
  method: "POST" | "DELETE";
  path: string;
  outcome: "success" | "failure" | "preview";
  timestamp: string;
  message: string;
  code: string;
  correlationId: string;
};

const ACTION_FEEDBACK_CAPACITY = 50;

const DAEMON_ACTION_CONFIG: Record<DaemonAction, DaemonActionConfig> = {
  start: {
    actionName: "daemon.start",
    label: "Start daemon",
    method: "POST",
    path: "/api/v1/daemon/start",
    risk: "low",
    destructive: false,
    impact: "Starts daemon processing.",
    plannedEffects: [
      "Daemon scheduler transitions to active processing.",
      "Queued runnable work becomes eligible for execution.",
    ],
    rollbackGuidance: "Use Pause daemon to stop scheduling new work.",
  },
  pause: {
    actionName: "daemon.pause",
    label: "Pause daemon",
    method: "POST",
    path: "/api/v1/daemon/pause",
    risk: "medium",
    destructive: false,
    impact: "Pauses new daemon scheduling work.",
    plannedEffects: [
      "New scheduling is paused while current runs can continue.",
      "Operators can safely inspect runtime state before resuming.",
    ],
    rollbackGuidance: "Use Resume daemon to continue scheduling.",
  },
  resume: {
    actionName: "daemon.resume",
    label: "Resume daemon",
    method: "POST",
    path: "/api/v1/daemon/resume",
    risk: "low",
    destructive: false,
    impact: "Resumes daemon scheduling work.",
    plannedEffects: [
      "Scheduler resumes polling and dispatching work.",
      "Paused task selection returns to normal cadence.",
    ],
    rollbackGuidance: "Use Pause daemon if scheduling must stop again.",
  },
  stop: {
    actionName: "daemon.stop",
    label: "Stop daemon",
    method: "POST",
    path: "/api/v1/daemon/stop",
    risk: "high",
    destructive: true,
    impact: "Stops the daemon and can interrupt active scheduling.",
    plannedEffects: [
      "Daemon lifecycle transitions toward stopped.",
      "Active scheduling loops may be interrupted.",
    ],
    irreversibleConsequence: "In-flight scheduling decisions can be interrupted immediately.",
    rollbackGuidance: "Use Start daemon to restart processing after stop completes.",
    confirmationPhrase: "STOP DAEMON",
  },
  clear: {
    actionName: "daemon.clear_logs",
    label: "Clear daemon logs",
    method: "DELETE",
    path: "/api/v1/daemon/logs",
    risk: "high",
    destructive: true,
    impact: "Permanently clears daemon log history shown in the UI.",
    plannedEffects: [
      "Daemon log entries are removed from API-backed history.",
      "UI log timeline is reset on next refresh.",
    ],
    irreversibleConsequence: "Cleared log entries cannot be restored from this UI session.",
    rollbackGuidance: "No rollback available. Export logs before clearing if retention is needed.",
    confirmationPhrase: "CLEAR DAEMON LOGS",
  },
};

export function matchesConfirmationPhrase(input: string, expected: string): boolean {
  return input.trim() === expected.trim();
}

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
  const [pendingExecution, setPendingExecution] = useState<{
    action: DaemonAction;
    correlationId: string;
  } | null>(null);
  const [actionRecords, setActionRecords] = useState<ActionFeedbackRecord[]>([]);
  const [actionState, setActionState] = useState<
    { kind: "idle" } | { kind: "ok"; message: string } | { kind: "error"; message: string }
  >({ kind: "idle" });
  const triggerButtonRef = useRef<HTMLButtonElement | null>(null);
  const confirmationInputRef = useRef<HTMLInputElement | null>(null);
  const pendingExecutionRef = useRef(false);
  const feedbackSequenceRef = useRef(0);
  const isMountedRef = useRef(true);

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

  useEffect(() => {
    if (!pendingAction) {
      return;
    }

    confirmationInputRef.current?.focus();
  }, [pendingAction]);

  useEffect(
    () => () => {
      isMountedRef.current = false;
    },
    [],
  );

  const closeConfirmationDialog = () => {
    setPendingAction(null);
    setConfirmationInput("");
    const trigger = triggerButtonRef.current;
    triggerButtonRef.current = null;
    if (trigger) {
      window.setTimeout(() => {
        trigger.focus();
      }, 0);
    }
  };

  const appendActionRecord = (record: Omit<ActionFeedbackRecord, "id">) => {
    if (!isMountedRef.current) {
      return;
    }

    feedbackSequenceRef.current += 1;
    const nextRecord: ActionFeedbackRecord = {
      id: `daemon-action-${feedbackSequenceRef.current}`,
      ...record,
    };

    setActionRecords((current) => {
      const next = [nextRecord, ...current];
      return next.length > ACTION_FEEDBACK_CAPACITY ? next.slice(0, ACTION_FEEDBACK_CAPACITY) : next;
    });
  };

  const clearPendingExecution = () => {
    pendingExecutionRef.current = false;
    if (isMountedRef.current) {
      setPendingExecution(null);
    }
  };

  const runAction = (action: DaemonAction) => {
    const config = DAEMON_ACTION_CONFIG[action];
    if (pendingExecutionRef.current) {
      setActionState({
        kind: "error",
        message: "Another action is already pending.",
      });
      return;
    }

    const correlationId = generateCorrelationId();
    pendingExecutionRef.current = true;
    if (isMountedRef.current) {
      setPendingExecution({
        action,
        correlationId,
      });
      setActionState({ kind: "idle" });
    }

    const request =
      action === "start"
        ? api.daemonStart({ correlationId })
        : action === "pause"
          ? api.daemonPause({ correlationId })
          : action === "resume"
            ? api.daemonResume({ correlationId })
            : action === "stop"
              ? api.daemonStop({ correlationId })
              : api.daemonClearLogs({ correlationId });

    void request
      .then((result) => {
        const timestamp = new Date().toISOString();

        if (result.kind === "error") {
          if (isMountedRef.current) {
            setActionState({ kind: "error", message: `${result.code}: ${result.message}` });
          }
          appendActionRecord({
            action: config.actionName,
            method: config.method,
            path: config.path,
            outcome: "failure",
            timestamp,
            message: result.message,
            code: result.code,
            correlationId: result.correlationId ?? correlationId,
          });
          clearPendingExecution();
          return;
        }

        const successMessage =
          typeof result.data.message === "string" && result.data.message.trim().length > 0
            ? result.data.message
            : `${config.label} completed.`;
        if (isMountedRef.current) {
          setActionState({ kind: "ok", message: successMessage });
        }
        appendActionRecord({
          action: config.actionName,
          method: config.method,
          path: config.path,
          outcome: "success",
          timestamp,
          message: successMessage,
          code: "ok",
          correlationId,
        });
        if (isMountedRef.current) {
          setRefreshNonce((current) => current + 1);
        }
        clearPendingExecution();
      })
      .catch((error) => {
        const message = error instanceof Error ? error.message : "Daemon action failed unexpectedly.";
        if (isMountedRef.current) {
          setActionState({ kind: "error", message });
        }
        appendActionRecord({
          action: config.actionName,
          method: config.method,
          path: config.path,
          outcome: "failure",
          timestamp: new Date().toISOString(),
          message,
          code: "request_rejected",
          correlationId,
        });
        clearPendingExecution();
      });
  };

  const requestAction = (action: DaemonAction, trigger?: HTMLButtonElement | null) => {
    if (pendingExecutionRef.current) {
      setActionState({
        kind: "error",
        message: "Another action is already pending.",
      });
      return;
    }

    const config = DAEMON_ACTION_CONFIG[action];
    if (config.risk !== "high") {
      runAction(action);
      return;
    }

    triggerButtonRef.current = trigger ?? null;
    setPendingAction(action);
    setConfirmationInput("");
    setActionState({ kind: "idle" });
  };

  const runDryRunPreview = () => {
    if (!pendingAction) {
      return;
    }

    const config = DAEMON_ACTION_CONFIG[pendingAction];
    const timestamp = new Date().toISOString();
    const correlationId = generateCorrelationId();
    const previewMessage = `Dry-run preview ready for ${config.label}.`;
    setActionState({
      kind: "ok",
      message: previewMessage,
    });
    appendActionRecord({
      action: config.actionName,
      method: config.method,
      path: config.path,
      outcome: "preview",
      timestamp,
      message: previewMessage,
      code: "dry_run",
      correlationId,
    });
  };

  const pendingConfig = pendingAction ? DAEMON_ACTION_CONFIG[pendingAction] : null;
  const canConfirm = pendingConfig?.confirmationPhrase
    ? matchesConfirmationPhrase(confirmationInput, pendingConfig.confirmationPhrase)
    : false;
  const pendingLabel = pendingExecution ? DAEMON_ACTION_CONFIG[pendingExecution.action].label : null;

  return (
    <RouteSection title="Daemon" description="Control daemon state, health, and log stream.">
      <div className="panel-actions">
        <button
          type="button"
          disabled={pendingExecution !== null}
          onClick={(event) => requestAction("start", event.currentTarget)}
        >
          Start Daemon
        </button>
        <button
          type="button"
          disabled={pendingExecution !== null}
          onClick={(event) => requestAction("pause", event.currentTarget)}
        >
          Pause Daemon
        </button>
        <button
          type="button"
          disabled={pendingExecution !== null}
          onClick={(event) => requestAction("resume", event.currentTarget)}
        >
          Resume Daemon
        </button>
        <button
          type="button"
          className="danger-action"
          disabled={pendingExecution !== null}
          onClick={(event) => requestAction("stop", event.currentTarget)}
        >
          Stop Daemon
        </button>
        <button
          type="button"
          className="danger-action"
          disabled={pendingExecution !== null}
          onClick={(event) => requestAction("clear", event.currentTarget)}
        >
          Clear Daemon Logs
        </button>
      </div>

      {pendingConfig ? (
        <div className="safeguard-dialog-backdrop">
          <section
            className="safeguard-dialog"
            role="dialog"
            aria-modal="true"
            aria-labelledby="safeguard-title"
            aria-describedby="safeguard-description"
            onKeyDown={(event) => {
              if (event.key === "Escape") {
                event.preventDefault();
                closeConfirmationDialog();
              }
            }}
          >
            <h2 id="safeguard-title">Review High-Risk Action</h2>
            <p id="safeguard-description">{pendingConfig.impact}</p>
            <p className="safeguard-risk">
              Risk classification: <code>{pendingConfig.risk}</code>
              {pendingConfig.destructive ? " (destructive)" : ""}
            </p>
            <div className="safeguard-preview">
              <h3>Preflight Preview</h3>
              <p>
                <strong>Planned request:</strong> <code>{pendingConfig.method}</code>{" "}
                <code>{pendingConfig.path}</code>
              </p>
              <p>
                <strong>Planned effects:</strong>
              </p>
              <ul className="safeguard-list">
                {pendingConfig.plannedEffects.map((effect) => (
                  <li key={effect}>{effect}</li>
                ))}
              </ul>
              {pendingConfig.irreversibleConsequence ? (
                <p className="safeguard-warning">
                  <strong>Irreversible consequence:</strong> {pendingConfig.irreversibleConsequence}
                </p>
              ) : null}
              {pendingConfig.rollbackGuidance ? (
                <p>
                  <strong>Rollback guidance:</strong> {pendingConfig.rollbackGuidance}
                </p>
              ) : null}
              <p>
                <strong>Current daemon health snapshot:</strong>
              </p>
              {healthState.status === "ready" || healthState.status === "empty" ? (
                <pre>{JSON.stringify(healthState.data, null, 2)}</pre>
              ) : healthState.status === "error" ? (
                <p>
                  Snapshot unavailable: {healthState.error.code}: {healthState.error.message}
                </p>
              ) : (
                <p>Loading snapshot...</p>
              )}
            </div>
          <p>
            <strong>Action:</strong> {pendingConfig.label}
          </p>
          <label>
            Confirmation phrase
            <input
              ref={confirmationInputRef}
              value={confirmationInput}
              onChange={(event) => setConfirmationInput(event.target.value)}
              placeholder={pendingConfig.confirmationPhrase}
              aria-describedby="confirmation-help"
            />
          </label>
          <p id="confirmation-help">
            Type <code>{pendingConfig.confirmationPhrase}</code> exactly (case-sensitive) to enable
            confirmation.
          </p>
          <div className="panel-actions">
            <button type="button" onClick={runDryRunPreview}>
              Run Dry-Run Preview
            </button>
            <button
              type="button"
              disabled={!canConfirm || pendingExecution !== null}
              onClick={() => {
                if (!pendingAction) {
                  return;
                }

                const config = DAEMON_ACTION_CONFIG[pendingAction];
                if (!config.confirmationPhrase) {
                  setActionState({
                    kind: "error",
                    message: "Safeguard metadata is missing confirmation requirements.",
                  });
                  return;
                }

                if (!matchesConfirmationPhrase(confirmationInput, config.confirmationPhrase)) {
                  setActionState({
                    kind: "error",
                    message: `Typed phrase must match ${config.confirmationPhrase} exactly.`,
                  });
                  return;
                }

                const actionToRun = pendingAction;
                closeConfirmationDialog();
                runAction(actionToRun);
              }}
            >
              Confirm and Execute
            </button>
            <button
              type="button"
              onClick={() => {
                closeConfirmationDialog();
              }}
            >
              Cancel
            </button>
          </div>
          </section>
        </div>
      ) : null}

      {pendingExecution ? (
        <p role="status" aria-live="polite">
          {pendingLabel} is in progress. Correlation ID: <code>{pendingExecution.correlationId}</code>
        </p>
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

      <div className="panel daemon-feedback-panel">
        <h2>Action Feedback</h2>
        <p>Daemon-scoped action feedback for this browser session (up to 50 entries).</p>
        {actionRecords.length === 0 ? (
          <EmptyState message="No daemon action outcomes recorded in this session." />
        ) : (
          <ul className="daemon-feedback-list">
            {actionRecords.map((record) => (
              <li key={record.id} className="daemon-feedback-item" data-outcome={record.outcome}>
                <p className="daemon-feedback-headline">
                  <strong>
                    {record.outcome === "success"
                      ? "Success"
                      : record.outcome === "failure"
                        ? "Failure"
                        : "Preview"}
                  </strong>{" "}
                  <code>{record.action}</code>
                </p>
                <p>
                  <time dateTime={record.timestamp}>{record.timestamp}</time>
                </p>
                <p>
                  <code>{record.method}</code> <code>{record.path}</code>
                </p>
                <p>
                  <strong>{record.code}</strong>: {record.message}
                </p>
                <p>
                  Correlation ID: <code>{record.correlationId}</code>
                </p>
              </li>
            ))}
          </ul>
        )}
      </div>

      <DiagnosticsPanel title="Daemon Diagnostics" actionPrefixes={["daemon."]} />

      <div className="grid two">
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
        render={(data) => <JsonPanel title="Requirement" data={data} />}
      />
    </RouteSection>
  );
}

export function TasksPage() {
  const state = useApiResource(
    async () => {
      const [tasks, stats] = await Promise.all([api.tasksList(), api.tasksStats()]);
      const error = firstApiError(tasks, stats);
      if (error) {
        return error;
      }

      return {
        kind: "ok" as const,
        data: {
          tasks: tasks.data,
          stats: stats.data,
        },
      };
    },
    [],
  );

  return (
    <RouteSection title="Tasks" description="Task explorer with stats and detail entry points.">
      <ResourceStateView
        state={state}
        emptyMessage="No tasks returned."
        render={(data) => (
          <div className="grid two">
            <JsonPanel title="Tasks" data={data.tasks} />
            <JsonPanel title="Task Stats" data={data.stats} />
          </div>
        )}
      />
    </RouteSection>
  );
}

export function TaskDetailPage() {
  const params = useParams();
  const taskId = params.taskId ?? "";

  const state = useApiResource(
    async () => api.tasksById(taskId),
    [taskId],
  );

  return (
    <RouteSection title="Task Detail" description={`Task ${taskId}.`}>
      <ResourceStateView
        state={state}
        emptyMessage="Task detail payload is empty."
        render={(data) => <JsonPanel title="Task" data={data} />}
      />
    </RouteSection>
  );
}

export function WorkflowsPage() {
  const state = useApiResource(
    async () => api.workflowsList(),
    [],
    {
      isEmpty: (data) => Array.isArray(data) && data.length === 0,
    },
  );

  return (
    <RouteSection title="Workflows" description="Workflow list and execution entrypoint.">
      <ResourceStateView
        state={state}
        emptyMessage="No workflow records returned."
        render={(data) => <JsonPanel title="Workflows" data={data} />}
      />
    </RouteSection>
  );
}

export function WorkflowDetailPage() {
  const params = useParams();
  const workflowId = params.workflowId ?? "";

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
    [workflowId],
  );

  return (
    <RouteSection title="Workflow Detail" description={`Workflow ${workflowId}.`}>
      <ResourceStateView
        state={state}
        emptyMessage="Workflow detail payload is empty."
        render={(data) => (
          <div className="grid two">
            <JsonPanel title="Workflow" data={data.workflow} />
            <JsonPanel title="Decisions" data={data.decisions} />
            <JsonPanel title="Checkpoints" data={data.checkpoints} />
          </div>
        )}
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
