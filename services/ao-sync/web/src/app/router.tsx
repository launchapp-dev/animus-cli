import { createBrowserRouter, Navigate } from "react-router-dom";
import { Layout } from "./layout";
import { LoginPage } from "./login";
import { SignupPage } from "./signup";
import { DashboardPage } from "./dashboard";
import { ProjectPage } from "./project";
import { TaskDetailPage } from "./task-detail";
import { RequirementDetailPage } from "./requirement-detail";
import { SettingsPage } from "./settings";

export const router = createBrowserRouter([
  { path: "/login", element: <LoginPage /> },
  { path: "/signup", element: <SignupPage /> },
  {
    path: "/",
    element: <Layout />,
    children: [
      { index: true, element: <Navigate to="/dashboard" replace /> },
      { path: "dashboard", element: <DashboardPage /> },
      { path: "projects/:projectId", element: <ProjectPage /> },
      { path: "projects/:projectId/tasks/:taskId", element: <TaskDetailPage /> },
      { path: "projects/:projectId/requirements/:reqId", element: <RequirementDetailPage /> },
      { path: "settings", element: <SettingsPage /> },
    ],
  },
]);
