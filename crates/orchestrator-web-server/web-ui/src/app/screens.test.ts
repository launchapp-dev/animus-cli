import { describe, expect, it } from "vitest";

const featureModules = [
  {
    name: "dashboard-page",
    load: () => import("./dashboard-page"),
    exports: ["DashboardPage"],
  },
  {
    name: "tasks-pages",
    load: () => import("./tasks-pages"),
    exports: ["TasksPage", "TaskCreatePage", "TaskDetailPage"],
  },
  {
    name: "workflow-pages",
    load: () => import("./workflow-pages"),
    exports: ["WorkflowsPage", "WorkflowDetailPage", "WorkflowCheckpointPage"],
  },
  {
    name: "queue-page",
    load: () => import("./queue-page"),
    exports: ["QueuePage"],
  },
  {
    name: "daemon-page",
    load: () => import("./daemon-page"),
    exports: ["DaemonPage"],
  },
  {
    name: "projects-pages",
    load: () => import("./projects-pages"),
    exports: ["ProjectsPage", "ProjectDetailPage", "RequirementDetailPage"],
  },
  {
    name: "events-page",
    load: () => import("./events-page"),
    exports: ["EventsPage"],
  },
  {
    name: "review-page",
    load: () => import("./review-page"),
    exports: ["ReviewHandoffPage"],
  },
  {
    name: "output-page",
    load: () => import("./output-page"),
    exports: ["TaskOutputPage"],
  },
  {
    name: "not-found-page",
    load: () => import("./not-found-page"),
    exports: ["NotFoundPage"],
  },
] as const;

describe("feature page modules", () => {
  for (const { name, load, exports: requiredExports } of featureModules) {
    it(`${name} exports renderable page components`, async () => {
      const module = await load();

      for (const exportName of requiredExports) {
        expect(module).toHaveProperty(exportName);
        expect(typeof module[exportName]).toBe("function");
      }
    });
  }
});
