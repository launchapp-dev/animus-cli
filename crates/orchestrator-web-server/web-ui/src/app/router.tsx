import { Suspense, lazy } from "react";
import type { ReactNode } from "react";
import { createBrowserRouter, Navigate, RouterProvider, useRouteError } from "react-router-dom";

import { AppShellLayout } from "./shell";

type ScreensModule = typeof import("./screens");
type ScreenExport =
  | "DashboardPage"
  | "DaemonPage"
  | "ProjectsPage"
  | "ProjectDetailPage"
  | "RequirementDetailPage"
  | "TasksPage"
  | "TaskDetailPage"
  | "WorkflowsPage"
  | "WorkflowDetailPage"
  | "WorkflowCheckpointPage"
  | "EventsPage"
  | "ReviewHandoffPage"
  | "NotFoundPage";

const lazyScreen = (name: ScreenExport) =>
  lazy(async () => import("./screens").then((module: ScreensModule) => ({ default: module[name] })));

const DashboardPage = lazyScreen("DashboardPage");
const DaemonPage = lazyScreen("DaemonPage");
const ProjectsPage = lazyScreen("ProjectsPage");
const ProjectDetailPage = lazyScreen("ProjectDetailPage");
const RequirementDetailPage = lazyScreen("RequirementDetailPage");
const TasksPage = lazyScreen("TasksPage");
const TaskDetailPage = lazyScreen("TaskDetailPage");
const WorkflowsPage = lazyScreen("WorkflowsPage");
const WorkflowDetailPage = lazyScreen("WorkflowDetailPage");
const WorkflowCheckpointPage = lazyScreen("WorkflowCheckpointPage");
const EventsPage = lazyScreen("EventsPage");
const ReviewHandoffPage = lazyScreen("ReviewHandoffPage");
const NotFoundPage = lazyScreen("NotFoundPage");

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
        element: withRouteSuspense(<DashboardPage />),
      },
      {
        path: "daemon",
        element: withRouteSuspense(<DaemonPage />),
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
        path: "tasks",
        element: withRouteSuspense(<TasksPage />),
      },
      {
        path: "tasks/:taskId",
        element: withRouteSuspense(<TaskDetailPage />),
      },
      {
        path: "workflows",
        element: withRouteSuspense(<WorkflowsPage />),
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
    <Suspense fallback={<section className="loading-box">Loading route...</section>}>
      {element}
    </Suspense>
  );
}
