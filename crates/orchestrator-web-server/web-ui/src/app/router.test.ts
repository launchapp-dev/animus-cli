import { describe, expect, it } from "vitest";

import { APP_ROUTE_PATHS } from "./router";

describe("APP_ROUTE_PATHS", () => {
  it("contains required route architecture", () => {
    const requiredPaths = [
      "/",
      "/dashboard",
      "/daemon",
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
      "/workflows",
      "/workflows/dispatch/task",
      "/workflows/dispatch/requirements",
      "/workflows/dispatch/custom",
      "/workflows/:workflowId",
      "/workflows/:workflowId/checkpoints/:checkpoint",
      "/queue",
      "/events",
      "/reviews/handoff",
      "*",
    ];

    expect(APP_ROUTE_PATHS).toEqual(requiredPaths);
  });
});
