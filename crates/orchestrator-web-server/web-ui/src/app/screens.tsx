import { FormEvent, ReactNode, useMemo, useState } from "react";
import { Link, useParams } from "react-router-dom";

import { useProjectContext } from "./project-context";
import { api, firstApiError, RequestJsonValue } from "../lib/api/client";
import { ApiError } from "../lib/api/envelope";
import { ResourceState, useApiResource } from "../lib/api/use-api-resource";
import { useDaemonEvents } from "../lib/events/use-daemon-events";

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

  const runAction = (action: "start" | "pause" | "resume" | "stop" | "clear") => {
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
        return;
      }

      setActionState({ kind: "ok", message: `Daemon action ${action} completed.` });
      setRefreshNonce((current) => current + 1);
    });
  };

  return (
    <RouteSection title="Daemon" description="Control daemon state, health, and log stream.">
      <div className="panel-actions">
        <button type="button" onClick={() => runAction("start")}>
          Start
        </button>
        <button type="button" onClick={() => runAction("pause")}>
          Pause
        </button>
        <button type="button" onClick={() => runAction("resume")}>
          Resume
        </button>
        <button type="button" onClick={() => runAction("stop")}>
          Stop
        </button>
        <button type="button" onClick={() => runAction("clear")}>
          Clear Logs
        </button>
      </div>

      {actionState.kind === "ok" ? <p>{actionState.message}</p> : null}
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
