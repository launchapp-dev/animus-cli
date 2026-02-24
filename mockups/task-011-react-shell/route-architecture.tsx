/**
 * TASK-011 wireframe scaffold.
 * This file is intentionally implementation-light and serves as a concrete
 * handoff target for the web-ui build phase.
 */

import { Outlet, Navigate, createBrowserRouter } from "react-router-dom";
import { type ReactNode } from "react";

type RouteState = "loading" | "ready" | "empty" | "error";
type ActionState = "idle" | "pending" | "success" | "failure";
type StreamState = "connecting" | "live" | "reconnecting" | "disconnected";
type TraceabilityId =
  | "AC-01"
  | "AC-02"
  | "AC-03"
  | "AC-04"
  | "AC-05"
  | "AC-06"
  | "AC-07"
  | "AC-08"
  | "AC-09";

type UiError = {
  code: string;
  message: string;
  exitCode: number;
};

type ApiResult<TData> =
  | { kind: "ok"; data: TData }
  | { kind: "error"; code: string; message: string; exitCode: number };

type ProjectContextValue = {
  activeProjectId: string | null;
  source: "route-param" | "cached-selection" | "server-active" | "none";
  setActiveProject: (projectId: string) => void;
};

type ResolveProjectContextInput = {
  routeProjectId: string | null;
  cachedProjectId: string | null;
  serverActiveProjectId: string | null;
  setActiveProject: (projectId: string) => void;
};

type RouteDescriptor = {
  path: string;
  screen: string;
  acceptance: TraceabilityId[];
};

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

export const routeCoverage: RouteDescriptor[] = [
  { path: "/", screen: "Root Redirect", acceptance: ["AC-01"] },
  { path: "/dashboard", screen: "Dashboard", acceptance: ["AC-01", "AC-07", "AC-08"] },
  { path: "/daemon", screen: "Daemon", acceptance: ["AC-01", "AC-04", "AC-07"] },
  { path: "/projects", screen: "Projects", acceptance: ["AC-01", "AC-05", "AC-07"] },
  {
    path: "/projects/:projectId",
    screen: "Project Detail",
    acceptance: ["AC-01", "AC-05", "AC-07"],
  },
  {
    path: "/projects/:projectId/requirements/:requirementId",
    screen: "Requirement Detail",
    acceptance: ["AC-01", "AC-05"],
  },
  { path: "/tasks", screen: "Tasks", acceptance: ["AC-01", "AC-03", "AC-07", "AC-08"] },
  { path: "/tasks/:taskId", screen: "Task Detail", acceptance: ["AC-01", "AC-03", "AC-04"] },
  { path: "/workflows", screen: "Workflows", acceptance: ["AC-01", "AC-03", "AC-07"] },
  {
    path: "/workflows/:workflowId",
    screen: "Workflow Detail",
    acceptance: ["AC-01", "AC-03", "AC-04"],
  },
  {
    path: "/workflows/:workflowId/checkpoints/:checkpoint",
    screen: "Checkpoint Detail",
    acceptance: ["AC-01", "AC-03"],
  },
  { path: "/events", screen: "Events", acceptance: ["AC-01", "AC-06", "AC-07"] },
  { path: "/reviews/handoff", screen: "Review Handoff", acceptance: ["AC-01", "AC-07"] },
  { path: "*", screen: "In-App Not Found", acceptance: ["AC-02"] },
];

export const acceptanceTraceability: Record<TraceabilityId, string[]> = {
  "AC-01": ["Route table covers root redirect plus all required paths."],
  "AC-02": ["Wildcard route renders app-level not found view while shell stays mounted."],
  "AC-03": ["Every route loader is expected to consume `parseAoEnvelope` output."],
  "AC-04": ["Error rendering path uses code/message/exitCode model from parsed envelope."],
  "AC-05": ["Project context resolver enforces route param -> cached -> server-active order."],
  "AC-06": ["Events route uses SSE and resumes from `Last-Event-ID` on reconnect."],
  "AC-07": ["Shell landmarks and route boundaries assume keyboard-first navigation."],
  "AC-08": ["Navigation and content are structured for 320px-first layouts."],
  "AC-09": ["`api_only` handling remains server-side and unchanged by client route tree."],
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

// Precedence order for project scope:
// 1) route param
// 2) cached selection
// 3) /api/v1/projects/active
function resolveProjectContext(input: ResolveProjectContextInput): ProjectContextValue {
  if (input.routeProjectId) {
    return {
      activeProjectId: input.routeProjectId,
      source: "route-param",
      setActiveProject: input.setActiveProject,
    };
  }

  if (input.cachedProjectId) {
    return {
      activeProjectId: input.cachedProjectId,
      source: "cached-selection",
      setActiveProject: input.setActiveProject,
    };
  }

  if (input.serverActiveProjectId) {
    return {
      activeProjectId: input.serverActiveProjectId,
      source: "server-active",
      setActiveProject: input.setActiveProject,
    };
  }

  return {
    activeProjectId: null,
    source: "none",
    setActiveProject: input.setActiveProject,
  };
}

function AppShellLayout(): ReactNode {
  return (
    <div className="app-shell">
      <header>
        {/* identity + breadcrumb + project selector + active project badge */}
      </header>
      <nav aria-label="Primary">{/* dashboard/daemon/projects/tasks/... */}</nav>
      <main>
        <Outlet />
      </main>
    </div>
  );
}

function RouteBoundary(props: {
  title: string;
  routeState: RouteState;
  actionState?: ActionState;
  streamState?: StreamState;
  error?: UiError;
  children?: ReactNode;
}): ReactNode {
  return (
    <section aria-label={props.title}>
      <h1>{props.title}</h1>
      {/* Wireframe-level state slots for loading/empty/error/action/stream. */}
      <p>
        state: {props.routeState}
        {props.actionState ? ` | action: ${props.actionState}` : ""}
        {props.streamState ? ` | stream: ${props.streamState}` : ""}
      </p>
      {props.error ? (
        <p>
          code={props.error.code}; message={props.error.message}; exit={props.error.exitCode}
        </p>
      ) : null}
      {props.children}
    </section>
  );
}

function DashboardPage(): ReactNode {
  return (
    <RouteBoundary title="Dashboard" routeState="ready">
      {/* /api/v1/system/info, /api/v1/daemon/status, /api/v1/projects/active, /api/v1/tasks/stats */}
    </RouteBoundary>
  );
}

function DaemonPage(): ReactNode {
  return (
    <RouteBoundary title="Daemon" routeState="ready" actionState="idle">
      {/* status + health + logs + start/stop/pause/resume actions */}
    </RouteBoundary>
  );
}

function ProjectsPage(): ReactNode {
  return (
    <RouteBoundary title="Projects" routeState="ready">
      {/* list/select projects; updates context */}
    </RouteBoundary>
  );
}

function ProjectDetailPage(): ReactNode {
  return (
    <RouteBoundary title="Project Detail" routeState="ready">
      {/* /api/v1/projects/:id + tasks/workflows/requirements */}
    </RouteBoundary>
  );
}

function RequirementDetailPage(): ReactNode {
  return (
    <RouteBoundary title="Requirement Detail" routeState="ready">
      {/* /api/v1/project-requirements/:projectId/:requirementId */}
    </RouteBoundary>
  );
}

function TasksPage(): ReactNode {
  return (
    <RouteBoundary title="Tasks" routeState="ready">
      {/* list + prioritized + next + stats */}
    </RouteBoundary>
  );
}

function TaskDetailPage(): ReactNode {
  return (
    <RouteBoundary title="Task Detail" routeState="ready" actionState="pending">
      {/* status/checklist/dependency/assignment operations */}
    </RouteBoundary>
  );
}

function WorkflowsPage(): ReactNode {
  return (
    <RouteBoundary title="Workflows" routeState="ready" actionState="idle">
      {/* list + run workflow */}
    </RouteBoundary>
  );
}

function WorkflowDetailPage(): ReactNode {
  return (
    <RouteBoundary title="Workflow Detail" routeState="ready" actionState="pending">
      {/* resume/pause/cancel + decisions + checkpoints */}
    </RouteBoundary>
  );
}

function WorkflowCheckpointPage(): ReactNode {
  return (
    <RouteBoundary title="Checkpoint Detail" routeState="ready">
      {/* checkpoint evidence and metadata */}
    </RouteBoundary>
  );
}

function EventsPage(): ReactNode {
  return (
    <RouteBoundary
      title="Events"
      routeState="ready"
      streamState="reconnecting"
      actionState="idle"
    >
      {/* SSE: /api/v1/events with Last-Event-ID resume */}
    </RouteBoundary>
  );
}

function ReviewHandoffPage(): ReactNode {
  return (
    <RouteBoundary title="Review Handoff" routeState="ready" actionState="idle">
      {/* POST /api/v1/reviews/handoff */}
    </RouteBoundary>
  );
}

function NotFoundPage(): ReactNode {
  return (
    <RouteBoundary
      title="Not Found"
      routeState="error"
      error={{
        code: "not_found",
        message: "Unknown route. Return to dashboard.",
        exitCode: 3,
      }}
    />
  );
}

export const wireframeRouter = createBrowserRouter([
  {
    path: "/",
    element: <AppShellLayout />,
    children: [
      { index: true, element: <Navigate to="/dashboard" replace /> },
      { path: "dashboard", element: <DashboardPage /> },
      { path: "daemon", element: <DaemonPage /> },
      { path: "projects", element: <ProjectsPage /> },
      { path: "projects/:projectId", element: <ProjectDetailPage /> },
      {
        path: "projects/:projectId/requirements/:requirementId",
        element: <RequirementDetailPage />,
      },
      { path: "tasks", element: <TasksPage /> },
      { path: "tasks/:taskId", element: <TaskDetailPage /> },
      { path: "workflows", element: <WorkflowsPage /> },
      { path: "workflows/:workflowId", element: <WorkflowDetailPage /> },
      {
        path: "workflows/:workflowId/checkpoints/:checkpoint",
        element: <WorkflowCheckpointPage />,
      },
      { path: "events", element: <EventsPage /> },
      { path: "reviews/handoff", element: <ReviewHandoffPage /> },
      { path: "*", element: <NotFoundPage /> },
    ],
  },
]);

// Shared envelope parser shape expected by all route loaders.
export function parseAoEnvelope<TData>(payload: unknown): ApiResult<TData> {
  if (!isRecord(payload) || payload["schema"] !== "ao.cli.v1") {
    return {
      kind: "error",
      code: "invalid_envelope",
      message: "Expected ao.cli.v1 envelope response.",
      exitCode: 1,
    };
  }

  if (payload["ok"] === true) {
    if (!("data" in payload)) {
      return {
        kind: "error",
        code: "invalid_envelope",
        message: "Envelope `ok=true` missing `data` field.",
        exitCode: 1,
      };
    }

    const success = payload as AoSuccessEnvelope<TData>;
    return { kind: "ok", data: success.data };
  }

  if (payload["ok"] === false) {
    const errorEnvelope = payload as Partial<AoErrorEnvelope>;
    const rawError = isRecord(errorEnvelope.error) ? errorEnvelope.error : {};

    return {
      kind: "error",
      code: typeof rawError["code"] === "string" ? rawError["code"] : "unknown_error",
      message:
        typeof rawError["message"] === "string"
          ? rawError["message"]
          : "Unknown API error from envelope.",
      exitCode: typeof rawError["exit_code"] === "number" ? rawError["exit_code"] : 1,
    };
  }

  return {
    kind: "error",
    code: "invalid_envelope",
    message: "Envelope `ok` flag must be boolean.",
    exitCode: 1,
  };
}

export const projectContextWireframe = resolveProjectContext({
  routeProjectId: "alpha-launch",
  cachedProjectId: "beta-hardening",
  serverActiveProjectId: "gamma-ops",
  setActiveProject: () => {
    // wireframe stub
  },
});
