import { createBrowserRouter, Navigate, RouterProvider, useRouteError } from "react-router-dom";

import { AppShellLayout } from "./shell";
import {
  DaemonPage,
  DashboardPage,
  EventsPage,
  NotFoundPage,
  ProjectDetailPage,
  ProjectsPage,
  RequirementDetailPage,
  ReviewHandoffPage,
  TaskDetailPage,
  TasksPage,
  WorkflowCheckpointPage,
  WorkflowDetailPage,
  WorkflowsPage,
} from "./screens";

export const APP_ROUTE_PATHS = [
  "/",
  "/dashboard",
  "/daemon",
  "/projects",
  "/projects/:projectId",
  "/projects/:projectId/requirements/:requirementId",
  "/tasks",
  "/tasks/:taskId",
  "/workflows",
  "/workflows/:workflowId",
  "/workflows/:workflowId/checkpoints/:checkpoint",
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
        element: <DashboardPage />,
      },
      {
        path: "daemon",
        element: <DaemonPage />,
      },
      {
        path: "projects",
        element: <ProjectsPage />,
      },
      {
        path: "projects/:projectId",
        element: <ProjectDetailPage />,
      },
      {
        path: "projects/:projectId/requirements/:requirementId",
        element: <RequirementDetailPage />,
      },
      {
        path: "tasks",
        element: <TasksPage />,
      },
      {
        path: "tasks/:taskId",
        element: <TaskDetailPage />,
      },
      {
        path: "workflows",
        element: <WorkflowsPage />,
      },
      {
        path: "workflows/:workflowId",
        element: <WorkflowDetailPage />,
      },
      {
        path: "workflows/:workflowId/checkpoints/:checkpoint",
        element: <WorkflowCheckpointPage />,
      },
      {
        path: "events",
        element: <EventsPage />,
      },
      {
        path: "reviews/handoff",
        element: <ReviewHandoffPage />,
      },
      {
        path: "*",
        element: <NotFoundPage />,
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
