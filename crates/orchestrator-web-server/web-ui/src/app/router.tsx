import { Suspense, lazy } from "react";
import type { ReactNode } from "react";
import { createBrowserRouter, Navigate, RouterProvider, useRouteError } from "react-router-dom";

import {
  PlanningEntryRedirectPage,
  PlanningRequirementCreatePage,
  PlanningRequirementDetailPage,
  PlanningRequirementsPage,
  PlanningVisionPage,
} from "./planning-screens";
import { AppShellLayout } from "./shell";

const DashboardPage = lazy(() => import("./dashboard-page").then((m) => ({ default: m.DashboardPage })));
const DaemonPage = lazy(() => import("./daemon-page").then((m) => ({ default: m.DaemonPage })));
const ProjectsPage = lazy(() => import("./projects-pages").then((m) => ({ default: m.ProjectsPage })));
const ProjectDetailPage = lazy(() => import("./projects-pages").then((m) => ({ default: m.ProjectDetailPage })));
const RequirementDetailPage = lazy(() => import("./projects-pages").then((m) => ({ default: m.RequirementDetailPage })));
const TasksPage = lazy(() => import("./tasks-pages").then((m) => ({ default: m.TasksPage })));
const TaskCreatePage = lazy(() => import("./tasks-pages").then((m) => ({ default: m.TaskCreatePage })));
const TaskDetailPage = lazy(() => import("./tasks-pages").then((m) => ({ default: m.TaskDetailPage })));
const WorkflowsPage = lazy(() => import("./workflow-pages").then((m) => ({ default: m.WorkflowsPage })));
const WorkflowDetailPage = lazy(() => import("./workflow-pages").then((m) => ({ default: m.WorkflowDetailPage })));
const WorkflowCheckpointPage = lazy(() => import("./workflow-pages").then((m) => ({ default: m.WorkflowCheckpointPage })));
const QueuePage = lazy(() => import("./queue-page").then((m) => ({ default: m.QueuePage })));
const EventsPage = lazy(() => import("./events-page").then((m) => ({ default: m.EventsPage })));
const ReviewHandoffPage = lazy(() => import("./review-page").then((m) => ({ default: m.ReviewHandoffPage })));
const WorkflowBuilderBrowsePage = lazy(() => import("./builder-pages").then((m) => ({ default: m.WorkflowBuilderBrowsePage })));
const WorkflowBuilderNewPage = lazy(() => import("./builder-pages").then((m) => ({ default: m.WorkflowBuilderNewPage })));
const WorkflowBuilderEditPage = lazy(() => import("./builder-pages").then((m) => ({ default: m.WorkflowBuilderEditPage })));
const AgentManagementPage = lazy(() => import("./agent-page").then((m) => ({ default: m.AgentManagementPage })));
const TaskOutputPage = lazy(() => import("./output-page").then((m) => ({ default: m.TaskOutputPage })));
const NotFoundPage = lazy(() => import("./not-found-page").then((m) => ({ default: m.NotFoundPage })));
const TaskDispatchPage = lazy(() => import("./dispatch-pages").then((m) => ({ default: m.TaskDispatchPage })));
const RequirementDispatchPage = lazy(() => import("./dispatch-pages").then((m) => ({ default: m.RequirementDispatchPage })));
const CustomDispatchPage = lazy(() => import("./dispatch-pages").then((m) => ({ default: m.CustomDispatchPage })));

export const APP_ROUTE_PATHS = [
  "/",
  "/dashboard",
  "/daemon",
  "/agents",
  "/projects",
  "/projects/:projectId",
  "/projects/:projectId/requirements/:requirementId",
  "/planning",
  "/planning/vision",
  "/planning/requirements",
  "/planning/requirements/new",
  "/planning/requirements/:requirementId",
  "/tasks",
  "/tasks/new",
  "/tasks/:taskId",
  "/tasks/:taskId/output",
  "/workflows",
  "/workflows/builder",
  "/workflows/builder/new",
  "/workflows/builder/:definitionId",
  "/workflows/dispatch/task",
  "/workflows/dispatch/requirements",
  "/workflows/dispatch/custom",
  "/workflows/:workflowId",
  "/workflows/:workflowId/checkpoints/:checkpoint",
  "/queue",
  "/events",
  "/reviews/handoff",
  "*",
] as const;

const router = createBrowserRouter([
  {
    path: "/",
    element: <AppShellLayout />,
    errorElement: <RouteErrorBoundary />,
    children: [
      {
        index: true,
        element: <Navigate to="/dashboard" replace />,
      },
      {
        path: "dashboard",
        element: withRouteSuspense(<DashboardPage />),
      },
      {
        path: "daemon",
        element: withRouteSuspense(<DaemonPage />),
      },
      {
        path: "agents",
        element: withRouteSuspense(<AgentManagementPage />),
      },
      {
        path: "projects",
        element: withRouteSuspense(<ProjectsPage />),
      },
      {
        path: "projects/:projectId",
        element: withRouteSuspense(<ProjectDetailPage />),
      },
      {
        path: "projects/:projectId/requirements/:requirementId",
        element: withRouteSuspense(<RequirementDetailPage />),
      },
      {
        path: "planning",
        element: <PlanningEntryRedirectPage />,
      },
      {
        path: "planning/vision",
        element: <PlanningVisionPage />,
      },
      {
        path: "planning/requirements",
        element: <PlanningRequirementsPage />,
      },
      {
        path: "planning/requirements/new",
        element: <PlanningRequirementCreatePage />,
      },
      {
        path: "planning/requirements/:requirementId",
        element: <PlanningRequirementDetailPage />,
      },
      {
        path: "tasks",
        element: withRouteSuspense(<TasksPage />),
      },
      {
        path: "tasks/new",
        element: withRouteSuspense(<TaskCreatePage />),
      },
      {
        path: "tasks/:taskId",
        element: withRouteSuspense(<TaskDetailPage />),
      },
      {
        path: "tasks/:taskId/output",
        element: withRouteSuspense(<TaskOutputPage />),
      },
      {
        path: "workflows",
        element: withRouteSuspense(<WorkflowsPage />),
      },
      {
        path: "workflows/builder",
        element: withRouteSuspense(<WorkflowBuilderBrowsePage />),
      },
      {
        path: "workflows/builder/new",
        element: withRouteSuspense(<WorkflowBuilderNewPage />),
      },
      {
        path: "workflows/builder/:definitionId",
        element: withRouteSuspense(<WorkflowBuilderEditPage />),
      },
      {
        path: "workflows/dispatch/task",
        element: withRouteSuspense(<TaskDispatchPage />),
      },
      {
        path: "workflows/dispatch/requirements",
        element: withRouteSuspense(<RequirementDispatchPage />),
      },
      {
        path: "workflows/dispatch/custom",
        element: withRouteSuspense(<CustomDispatchPage />),
      },
      {
        path: "workflows/:workflowId",
        element: withRouteSuspense(<WorkflowDetailPage />),
      },
      {
        path: "workflows/:workflowId/checkpoints/:checkpoint",
        element: withRouteSuspense(<WorkflowCheckpointPage />),
      },
      {
        path: "queue",
        element: withRouteSuspense(<QueuePage />),
      },
      {
        path: "events",
        element: withRouteSuspense(<EventsPage />),
      },
      {
        path: "reviews/handoff",
        element: withRouteSuspense(<ReviewHandoffPage />),
      },
      {
        path: "*",
        element: withRouteSuspense(<NotFoundPage />),
      },
    ],
  },
]);

export function AppRouterProvider() {
  return <RouterProvider router={router} />;
}

function RouteErrorBoundary() {
  const error = useRouteError();

  return (
    <section className="panel" role="alert">
      <h1>Route Error</h1>
      <p>
        The route failed to render. Check endpoint responses and retry navigation.
      </p>
      <pre>{JSON.stringify(error, null, 2)}</pre>
    </section>
  );
}

function withRouteSuspense(element: ReactNode) {
  return (
    <Suspense
      fallback={(
        <section className="loading-box" role="status" aria-live="polite" aria-atomic="true">
          Loading route...
        </section>
      )}
    >
      {element}
    </Suspense>
  );
}
